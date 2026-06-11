 Detailed Technical Review: feat/zero-copy-architecture

       This branch makes three large bets at once: a custom WAL RMGR, a 64-way partitioned pending list, and removal of overflow
       pages. All three have correctness defects, and the merge path is unimplemented. This branch is not mergeable as-is and will
       lose or corrupt data on first crash, first large value, and first VACUUM after threshold.

       ---
       1. Correctness Issues (critical — data loss / corruption)

       1.1 roaring_merge_pending does nothing — pending list grows without bound, then resets to empty (DATA LOSS)

       File: src/roaring_vacuum.c:92-206

       The merge body is replaced with a TODO. The actual sequence in code:

       2. Records pending_merging_head[p] = old_heads[p] (GenericXLog #1).
       3. collect_pending reads entries into memory (only committed/own-xid ones).
       4. Allocates a fresh empty pending page per partition; swaps pending_insert_head[p]/tail[p]/count[p] to point to the empty
       pages (GenericXLog #2).
       5. pfree(entries); return;

       The collected entries are dropped on the floor. The old pending pages are leaked (no truncation, no merge into leaves,
       pending_merging_head[p] left pointing at the original head forever). Any value that was in the pending list before the merge
        is gone from query results until ambulkdelete is re-run; there is no fallback path that drains the carry/merging head.
       ambulkdelete, ambulkdelete_lossy, and amvacuumcleanup are all stubs returning NULL/stats (roaring_vacuum.c:208-224), so the
       inline leaf-bitmap modification described in CLAUDE.md does not exist on this branch either.

       Severity: catastrophic. Once total_pending >= pending_merge_threshold (default 10000), the next insert calls
       roaring_merge_pending, which silently discards every queued value. Scans will return empty for any key whose only TIDs were
       pending. This is silent data loss on the hot path.

       1.2 pending_merging_head is left non-Invalid forever → all future merges are no-ops

       File: src/roaring_vacuum.c:114-118

       if (meta->pending_merging_head[0] != InvalidBlockNumber)
       {
           UnlockReleaseBuffer(metabuf);
           return; /* another merger active */
       }

       The merge sets pending_merging_head[p] = old_heads[p] but never clears it. After the first merge, partition 0's slot stays
       non-Invalid, so this guard returns immediately on every subsequent merge. Combined with 1.1, even the (broken) merge stops
       running.

       Also: this guard only checks [0] but writes all [p]. Single-partition check, multi-partition write — inconsistent.

       1.3 Custom WAL redo skips entry_count >= ROARING_PENDING_PER_PAGE check; replays past page end

       File: src/roaring_wal.c:21-39

       The redo function unconditionally writes:

       RoaringPendingEntry *slot = (RoaringPendingEntry *) PageGetContents(tailpage) + spc->entry_count;
       *slot = xlrec->entry;
       ...
       spc->entry_count++;
       ((PageHeader) tailpage)->pd_lower += sizeof(...);

       There is no bounds check. If the primary inserted into slot 507 (last slot, entry_count=508 cap), and a torn-page or
       partial-recovery scenario causes the standby to replay the same record without applying the primary's full-page image, the
       slot index can exceed ROARING_PENDING_PER_PAGE. More importantly, pd_lower is incremented blindly — if the page LSN doesn't
       gate the redo correctly, pd_lower will exceed pd_upper and corrupt the page header. There is no validation that
       xlrec->tail_blkno matches the buffer's block number, no validation that partition is in range, and no validation that the
       page type is PENDING_INSERT. A misdirected WAL record (e.g. a developer bug that registers the wrong buffer) silently
       corrupts whatever is at block 0 of the index.

       1.4 Custom WAL: XLogReadBufferForRedo for tail page can never observe RBM_ZERO semantics on a torn-extension crash

       File: src/roaring_wal.c + src/roaring_insert.c:114-179

       The new-page (tail extension) path still uses GenericXLog (roaring_insert.c:145-174), but for any sequence of
       XLOG_ROARING_INSERT records that arrive between the GenericXLog extension record and a checkpoint, the redo function is the
       only way the standby learns to bump entry_count. If the extension's GenericXLog FPI is present and the subsequent inserts
       are replayed, that works. But if recovery starts mid-stream from a base backup taken between the extension and the next
       checkpoint, the tail page can be missing entirely. XLogReadBufferForRedo(record, 0, &tailbuf) will return BLK_NOTFOUND — and
        the current code silently ignores that return value, doing nothing for the tail and still incrementing
       pending_insert_count[partition] on the metapage. Result: count drifts; one phantom entry per missing tail page; scan will
       see a count > number of entries.

       For correctness, the redo function must:
       - Handle BLK_NOTFOUND and BLK_RESTORED explicitly.
       - Use XLogInitBufferForRedo if the record carries a "new page" flag.
       - Refuse to increment the meta counter if the tail apply was skipped.

       1.5 Insert path violates buffer-locking order convention (deadlock risk)

       File: src/roaring_insert.c:189-303

       The new-page extension path holds metabuf EXCLUSIVE → tailbuf EXCLUSIVE → newbuf EXCLUSIVE (:108, :121). The common-case
       (non-extend) path acquires metabuf EXCLUSIVE → tailbuf EXCLUSIVE (:202-206). The pending-page-scan path in
       pending_chain_as_bitmap (roaring_scan.c:171-191) acquires only tailbuf SHARE after releasing metabuf — fine.

       But roaring_merge_pending (roaring_vacuum.c:109-110, 156-186) holds metabuf EXCLUSIVE while calling roaring_extend_page
       repeatedly — which itself locks new buffers. Meanwhile, collect_pending (:46-82) is called between the GenericXLog regions
       on metapage, releasing and re-acquiring meta, and SHARE-locks pending pages. The interleaving is inconsistent: in some
       places the partitioned tail pages are locked while metapage is held exclusive; in others metapage is grabbed after tail is
       held. The author even left a note acknowledging the issue at roaring_insert.c:199:

       /* Unlock tailbuf first to prevent LWLock deadlock (metabuf must be locked before tailbuf) */

       …which contradicts the comment at the top of the function (step 3: lock tail page first without metapage). The current code
       does goto retry enough times to mask it usually, but with 64 partitions × concurrent merge × extension, the lock-order graph
        has cycles. There is no documented lock order. Under load, this WILL deadlock.

       1.6 roaring_pending_append Step-4 has a TOCTOU window that miscounts on contended pages

       File: src/roaring_insert.c:189-303

       The fast path:
       1. Read meta SHARE → cache tail_blkno, release meta (:51-65).
       2. Lock tail EX (:86-87); if full → fall to extension branch.
       3. Release tail (:200), lock meta EX, re-lock tail EX (:202-206).
       4. Re-check entry_count >= PER_PAGE (:209); if full → retry.
       5. Append; emit WAL; release.

       Between steps 2 and 3 (the unlock at line 200), another backend mapped to the same partition can fully fill the page (since
       MyProcPid % 64 collisions are common, see §3.1) or another backend can extend a new tail and swap
       pending_insert_tail[partition]. The re-check at :209 only checks if entry_count is full on the same tail_blkno — but if
       tail_blkno was swapped, we are now appending to an orphaned old page that is no longer the tail. Its entry_count may still
       be below PER_PAGE, so we happily append. Effects:
       - The entry is written into a page that is no longer reachable from pending_insert_head[partition] chain (if the swapper
       also unlinked, which they do via setting next_page from the old tail to new — but the old tail was linked, so it's still
       chained).
       - The increment to pending_insert_count[partition] is correct, but the value sits at the middle of the chain, not the tail.
       The value_min/value_max of the orphan tail is updated, but readers using the chain walk will still find it. So this
       particular variant is probably benign in terms of correctness if the chain is always walked head→tail.

       However, the deeper issue: the comment at :183-186 says:

       /* Another backend extended the page? Actually impossible since we held metapage. */

       This is wrong. The extension path holds metapage EX, but the normal append in step 4 of another concurrent backend does not
       hold metapage between steps 2 and 3 — only briefly at the end. The "impossible" assertion is false; only correct because the
        metapage EX in this branch's step 5 serializes with the metapage EX of another backend's step 4. That's a coincidence, not
       a design.

       1.7 GenericXLog fallback path in roaring_pending_append corrupts page contents

       File: src/roaring_insert.c:265-301

       When opts->custom_wal == false, the GenericXLog path does:

       tailimg = GenericXLogRegisterBuffer(state, tailbuf, 0);
       tailspc = (RoaringPendingSpecial *) PageGetSpecialPointer(tailimg);
       slot    = (RoaringPendingEntry *) PageGetContents(tailimg) + tailspc->entry_count;
       *slot = *newentry;
       ...
       tailspc->entry_count++;
       ((PageHeader) tailimg)->pd_lower += sizeof(RoaringPendingEntry);

       Per your MEMORY.md note (feedback_pg18_pd_lower): GenericXLog masks the "unused" region between pd_lower and pd_upper and
       treats it as garbage; only the prefix [0, pd_lower) and the special area [pd_upper, BLCKSZ) are tracked for diff. But here
       the entry is written into the unused region before pd_lower is bumped (well, after — the bump happens after). Look at the
       order:

       1. *slot = *newentry;                  // write at offset pd_lower (in the "unused" zone at this instant)
       2. tailspc->entry_count++;             // write in special — tracked
       3. ((PageHeader) tailimg)->pd_lower += sizeof(...);  // bump pd_lower

       Steps 1, 2, 3 all occur on the registered image (a copy). When GenericXLogFinish diffs, it computes the new diff range using
        the new pd_lower, so the slot bytes are inside [0, new pd_lower) and ARE included. OK, this is fine for the diff itself.

       BUT: there's a subtle defect — between steps 2 and 3, if entry_count++ actually changed bytes in special that xlog masks,
       fine. The real defect: on the primary, this only modifies tailimg, not BufferGetPage(tailbuf) directly — GenericXLogFinish
       will memcpy the modified image back to the buffer and emit WAL. OK.

       The actual concern: in the custom WAL path (:217-261), the code does NOT go through GenericXLog. It modifies
       BufferGetPage(tailbuf) directly, then XLogRegisterBuffer(0, tailbuf, REGBUF_STANDARD). Because REGBUF_STANDARD tells xlog to
        use pd_lower/pd_upper for hole compression, this only works if pd_lower is updated before XLogInsert. It is. Good. But the
       modifications happen outside a critical section before line 218 where START_CRIT_SECTION() is called. Wait —
       START_CRIT_SECTION() is at :218 and the modifications start at :221. That's fine.

       However: there is no MarkBufferDirty(metabuf) before the XLogInsert in the case where RelationNeedsWAL(index) is false —
       actually there is at :240. But PageSetLSN(metabuf) only runs inside the RelationNeedsWAL branch (:258), which is correct.
       OK, this part is mostly fine.

       The real defect in this block: pd_lower is incremented on the buffer page but the WAL record is xl_roaring_insert which
       carries the entry but does NOT carry pd_lower. On redo, the redo function (roaring_wal.c:35) also bumps pd_lower +=
       sizeof(RoaringPendingEntry). So if the redo runs against a page that already has the entry applied (because of an FPI from a
        different record, or because the LSN check failed to short-circuit), we'd double-bump. The XLogReadBufferForRedo LSN check
       should prevent this — but only if PageSetLSN(BufferGetPage(tailbuf), recptr) actually ran. Looking at :257-258, it runs only
        when RelationNeedsWAL(index). That's right. OK on that front.

       The genuine remaining issue: the custom WAL record uses REGBUF_STANDARD (:252-253), which omits the "hole" between pd_lower
       and pd_upper. But the record carries no FPI. If the page was never WAL-logged before this record (e.g. a torn write that
       lost the GenericXLog full-page image at extension time), redo cannot reconstruct it. The new-page extension uses
       GENERIC_XLOG_FULL_IMAGE (:155), so the page exists in WAL once. After that, append records carry no FPI. If recovery starts
       after the extension but the extension's WAL was lost (e.g. base backup taken mid-stream), XLogReadBufferForRedo returns
       BLK_NOTFOUND or finds a wrong-version page — and 1.4 above bites.

       1.8 Build path: max_tid chunking estimation is wrong and produces overlapping/buggy bitmaps

       File: src/roaring_build.c:241-313

       The binary search uses est_size = (mid - start_rank + 1) * 2 + 100. The true serialized size of a roaring32 container
       depends on container type (array: 2 bytes/element; bitmap: 8KB/65k range; run: varies). For dense ranges this estimate is
       wildly wrong (often 4× off), and the binary search is then "validated" by calling roaring_bitmap_copy + remove_range +
       portable_size. This is O(card²) over the cardinality space because each binary search step does roaring_bitmap_copy and two
       range-removes on the entire bitmap. For a million-TID value, this is gigabytes of unnecessary work — and that's the build
       hot path for high-cardinality keys.

       But the correctness defect: at :303-307, the chunk is materialised with:

       chunk = roaring_bitmap_copy(bm);
       if (best_val < 0xFFFFFFFF)
           roaring_bitmap_remove_range_closed(chunk, best_val + 1, 0xFFFFFFFF);
       if (start_val > 0)
           roaring_bitmap_remove_range_closed(chunk, 0, start_val - 1);

       Then:

       start_rank = best_idx + 1;

       start_rank is a rank (positional index), but start_val for the next iteration is computed as select(bm, start_rank). If the
       bitmap has gaps (e.g. {1, 5, 100}, ranks 0..2), going from rank 1 to rank 2 advances start_val from 5 to 100. The previous
       chunk removed everything > best_val where best_val = select(rank=1) = 5. So the next chunk starts at start_val = 100,
       removes [0, 99]. OK, that's correct.

       But consider: best_idx may equal start_rank (single-element chunk). Then start_rank = best_idx + 1 advances by 1. Fine. The
       off-by-one is OK.

       The real bug: the leaf entry for two consecutive chunks of the same value (same cur_value) both get written as separate
       RoaringLeafEntry rows with the same value but different max_tid. Scan does the right thing in lookup_and_combine_value
       (roaring_scan.c:218-258) — it iterates while e->value == v and ORs all chunks. But roaring_dir_lookup uses (value, max_tid)
       ordering. If a key's bitmap is split across leaf pages such that the chunks land on different leaves, roaring_dir_lookup
       with tid=0 finds the leaf containing the first chunk only. The scanner then walks right via right_page. OK, that mostly
       works.

       However: if there are two distinct values and one of them has multi-chunk entries on a page that filled up, the next page's
       leftmost entry might still be the same value. The directory's high_key is the last entry's value (:331-333, :409-411) —
       which is the same cur_value. So the directory entry for leaf N and leaf N+1 may both have high_key = cur_value. The
       directory binary search at roaring_scan.c:50 uses (high_key, max_tid) ordering — but the next leaf might contain the
       next-value chunks plus a tail chunk of the previous value. Reading max_tid from the dir entry of leaf N gives the
       prev-value's max_tid; tying-break by max_tid in the comparator is then comparing prev-value's TID range to current scan's
       tid=0. This is plausibly incorrect for boundary cases.

       1.9 Overflow path eliminated; build truncates bitmaps > 7.5 KB silently (DATA LOSS)

       File: src/roaring_build.c:241-384 + include/pg_roaring_index.h:60

       A roaring32 container maxes around 8KB (the bitmap container is fixed 8KB for 65,536 elements; array containers up to ~131
       KB for 4096 elements at 2 bytes each plus overhead, but they convert to bitmap above 4096 elements). For a key with even a
       single 65k-range chunk, the serialized container is 8 KB + ~10 bytes of header. That alone overflows
       ROARING_MAX_INLINE_BYTES = 7500. The chunking loop's binary search will repeatedly fail to find any best_idx satisfying the
       constraint; eventually right < left and the loop exits with best_idx = start_rank (the seed value), producing a 1-element
       chunk. So for keys with millions of TIDs, the build emits millions of single-TID leaf entries — and then PageAddItem
       (:375-377) fails: a leaf page holds at most ~500 of those. The leaf-page-full branch (:316) extends a new page, but for,
       say, 6500 TIDs/value × 1 entry/chunk × 16 bytes per leaf entry plus item-id overhead, that's ~104 KB per value, hundreds of
       leaf pages per value. Build will likely succeed but be monstrously slow and produce a directory that overflows two levels
       (the elog(ERROR, "index too large for two-level directory") at :444 will fire on any non-trivial dataset).

       Worse, in the merge path (which is unimplemented anyway), there is no chunking logic. So once merge is implemented naively,
       any value whose merged bitmap is > 7500 bytes will either overflow the page (PageAddItem fail → ERROR) or be silently
       truncated.

       1.10 roaring_bitmap_free called on a roaring_bitmap_frozen_view (memory corruption)

       File: src/roaring_scan.c:242, 439, 505

       const roaring_bitmap_t *view = roaring_bitmap_frozen_view((const char *)e + RoaringLeafEntryDataOffset, bitmap_len);
       roaring_bitmap_or_inplace(full_bm, view);
       roaring_bitmap_free(view);   // BUG

       roaring_bitmap_frozen_view returns a view that wraps a caller-owned buffer. The correct API is roaring_bitmap_free only if
       the view was created via roaring_bitmap_portable_deserialize (which allocates). For a frozen view, you must use
       roaring_bitmap_free(view) only on the wrapper allocated by frozen_view — and CRoaring's frozen_view does heap-allocate a
       roaring_bitmap_t shell, so roaring_bitmap_free is in fact correct for it, but you must NOT call any mutating operation on
       it. roaring_bitmap_or_inplace(full_bm, view) is fine because view is on the RHS. OK, this one is actually correct — but
       worth flagging in case anyone refactors.

       The cast (roaring_bitmap_t *) discards const later in the file (:343, :361) which would be a real bug if any of those
       bitmaps came from frozen_view. Trace: lookup_and_combine_value returns full_bm which is a fresh roaring_bitmap_create() ORed
        with frozen views — so it's a regular bitmap. OK, fine.

       1.11 Insert lock-ordering inversion (DEADLOCK)

       File: src/roaring_insert.c:200-206

       Step 4 explicitly says:

       /* Unlock tailbuf first to prevent LWLock deadlock (metabuf must be locked before tailbuf) */
       UnlockReleaseBuffer(tailbuf);

       metabuf = ReadBuffer(index, ROARING_METAPAGE_BLKNO);
       LockBuffer(metabuf, BUFFER_LOCK_EXCLUSIVE);

       tailbuf = ReadBuffer(index, tail_blkno);
       LockBuffer(tailbuf, BUFFER_LOCK_EXCLUSIVE);

       OK: meta first, then tail. But the extension path (Step 5, :121-122) calls roaring_extend_page while still holding metabuf
       EX and never having released tailbuf. Then it locks newbuf inside ExtendBufferedRel(EB_LOCK_FIRST). So the order is meta →
       tail → newbuf. Meanwhile, a concurrent reader in pending_chain_as_bitmap locks tail SHARE with no metabuf held — also fine.
       But if two backends both hit the extension path on different partitions concurrently, they both want meta EX — only one
       wins, the other blocks. Fine.

       The actual cycle: roaring_merge_pending holds meta EX and calls roaring_extend_page (vacuum.c:160), which extends and
       EX-locks the new page. A concurrent insert in the non-extending fast path holds meta EX (:203) and then EX-locks tail_blkno
       (:206). If tail_blkno happens to equal a block another extender just allocated (impossible during a single tx but possible
       across the partition swap during merge), you have a cycle. Probably not exploitable in practice, but the lock order is
       undocumented and roaring_merge_pending extending the relation while holding the metapage is a long-pole stall for ALL
       inserters across all 64 partitions. Defeats the purpose of partitioning.

       1.12 RoaringPendingSpecial xmin_low comparison uses signed semantics on wraparound (subtle)

       File: src/roaring_wal.c:29-30, roaring_insert.c:226-227

       TransactionIdPrecedes is the correct wraparound-safe comparator. Both call sites use it correctly. OK.

       1.13 lookup_and_combine_value: leak / double-free if the loop exits with leafbuf valid

       File: src/roaring_scan.c:206-262

       The local variable shadowing at :210 (Buffer leafbuf = ReadBuffer(...)) creates a new leafbuf inside the if-block, shadowing
        the outer leafbuf declared at :206. The outer leafbuf = InvalidBuffer is never used (it's shadowed); when the block exits,
       the inner leafbuf is the one in scope. The final if (leafbuf != InvalidBuffer) UnlockReleaseBuffer(leafbuf) at :260 refers
       to the inner variable — that's a happy accident due to scoping. But if leafbuf was set to InvalidBuffer and then leafbuf =
       ReadBuffer(...) on the next iteration (:253-254), the previous buffer was already released at :248. OK, this also works.

       The real issue: when next_blkno == InvalidBlockNumber, we break; (:251) but leafbuf was already set to InvalidBuffer at :249
        — so the trailing if (leafbuf != InvalidBuffer) correctly skips. OK.

       This code is correct but fragile (shadowing the outer variable saves it). Refactor to remove the shadow.

       1.14 roaring_redo is called outside any critical section / with no error handling

       File: src/roaring_wal.c:10-58

       If BufferGetPage(tailbuf) is corrupt (e.g. wrong page_type) and spc->entry_count is wild, the redo will compute a slot
       pointer past the buffer and segfault, killing recovery. There is no PageInit check, no sanity validation. Compare with
       ginRedoInsert in PG source — it validates and logs corruption clearly.

       ---
       4. WAL Design Review

       Verdict: the custom RMGR is the wrong call.

       Saved bytes per insert (custom vs GenericXLog):
       - Custom: xl_roaring_insert = 24 bytes + xlog header (~30 bytes) + 2 buffer refs (~10 bytes each with no FPI) = ~74 bytes.
       - GenericXLog with two buffers, only the delta region for each: header ~30 bytes + 2 buffer descriptors + diff. The diff for
        the tail-page change is sizeof(RoaringPendingEntry) = 16 bytes + 4 bytes (entry_count, value_min/max changes) +
       offset/length overhead ~24 bytes ≈ 40 bytes. Meta diff: sizeof(uint32) for the count = ~28 bytes. Total ~100-110 bytes.

       Net savings: ~30 bytes per insert. At 1M inserts/sec, that's 30 MB/s of WAL — meaningful, but achievable in other ways.

       Specific defects in the RMGR implementation

       1. No FPI strategy. Custom WAL records must self-document when FPI is needed. The tail page should be registered with
       REGBUF_WILL_INIT for the extension case (it's not — that goes through GenericXLog). For the append case, FPIs are taken
       automatically by XLogRegisterBuffer after a checkpoint thanks to full_page_writes=on. OK on that front, but you have to
       trust the LSN check in redo gates correctly — which (per §1.3, §1.4) it does not validate.
       2. Buffer registration order vs payload. Block 0 = tail, Block 1 = metapage. Redo applies in order (tail first). That's fine
        because they're independent. But the redo does if (BufferIsValid(metabuf)) UnlockReleaseBuffer(metabuf) without checking it
        was actually acquired — XLogReadBufferForRedo initializes the buffer parameter only on success. Reading uninitialized stack
        is UB. Should be metabuf = InvalidBuffer; tailbuf = InvalidBuffer; at the top.
       3. rm_decode == NULL. Logical decoding will fail loudly on any system using logical replication. For a custom RMGR
       registered with RegisterCustomRmgr, this means any pg_logical/pg_recvlogical consumer will choke on roaring index WAL. The
       user's MEMORY.md mentions a production workload — this likely matters.
       4. rm_mask == NULL. wal_consistency_checking will not work for the roaring RMGR. You lose the most important debugging tool
       for WAL bugs. Add a mask function that zeroes the unused [pd_lower, pd_upper) hole and any uninitialized pending entry
       slots.
       5. Custom RMGR registration races. _PG_init registers the rmgr only if (process_shared_preload_libraries_in_progress). So
       users MUST add pg_roaring_index to shared_preload_libraries, AND existing indexes built without it will fail recovery if WAL
        records exist before the library loads. There is no startup check that aborts cleanly if the library isn't preloaded but
       custom WAL was enabled.
       6. custom_wal is a per-index reloption. Two indexes in the same cluster can have different settings — one emits custom WAL,
       the other emits GenericXLog. Replication consumers must handle both. Fine, but undocumented.

       Recommendation on WAL

       Drop the custom RMGR. The 30 bytes/insert is not worth the maintenance burden, the logical-decoding incompatibility, and the
        correctness defects. If you need the throughput, the more impactful win is batching multiple pending appends into one WAL
       record at the application layer — i.e. use the aminsert_cleanup callback (PG17+ has aminsertcleanup) to flush a per-backend
       buffer of N pending entries in a single GenericXLog with N×16 bytes of payload. That cuts xlog headers/buffer descriptors by
        N× without a custom RMGR.

       If you keep the custom RMGR:
       - Add a rm_mask function for wal_consistency_checking.
       - Add a rm_decode for logical replication.
       - Validate BLK_NOTFOUND / BLK_RESTORED in redo and refuse to apply the meta increment if tail apply was skipped.
       - Validate partition < ROARING_MAX_PARTITIONS and page type in redo.
       - Initialize metabuf/tailbuf to InvalidBuffer at the top of roaring_redo.
       - Bounds-check entry_count against ROARING_PENDING_PER_PAGE before write.

       ---
       3. Partitioned Pending List Review

       3.1 MyProcPid % partitions_count is a bad hash

       PIDs on Linux are roughly sequential within a short window. A workload with a fixed pool of pgbouncer connections will reuse
        a small set of PIDs and bucket-skew massively. PIDs on macOS and Linux can range up to pid_max (typically 4M on Linux), but
        in practice a connection-pooler workload sees PIDs in a narrow band. With 64 partitions and 64 concurrent backends,
       expected collisions per partition by birthday paradox: ~20 backends on the most-loaded partition. That's not the
       order-of-magnitude reduction in contention you want.

       Better alternatives:
       - (MyProcNumber) % partitions — MyProcNumber is a dense [0..MaxBackends) index, used by all PG locking infrastructure.
       - pg_prng_uint32(&pg_global_prng_state) % partitions per insert — round-robins through partitions and amortizes any single
       hot partition. Slightly worse cache locality, but eliminates skew.

       Failure mode: PgBouncer in transaction-pooling mode with a fixed pool of, say, 32 worker PIDs maps to at most 32 partitions,
        half the array unused. Worse, if those 32 PIDs happen to be even (typical), and you use % 64, you get 32 partitions with 1
       backend each — best case. If the PIDs cluster modulo small powers of 2 (which they do — Linux PID allocation increments by
       1, but spawning N forks gives N adjacent PIDs), % 64 distributes them well. So this is workload-dependent and unpredictable.

       Recommendation: Use MyProcNumber % partitions_count. Available from miscadmin.h.

       3.2 Metapage size — fits, but barely

       Computed: RoaringMetaPageData = 1344 bytes, total page = 24 + 1344 = 1368 bytes. Plenty of room (BLCKSZ=8192). Fine.

       But: changing ROARING_MAX_PARTITIONS later breaks on-disk format. The arrays are sized at the maximum, not at
       partitions_count. Old indexes built with 64 will be read by code expecting 64 — OK as long as you never reduce the macro.
       But version bump to 4 catches mismatches. Acceptable.

       3.3 Merge semantics across 64 partitions: who merges and what prevents double-merge

       roaring_pending_append checks total_pending >= merge_threshold and calls roaring_merge_pending inline (insert.c:68-83).
       roaring_merge_pending takes metabuf EX and checks pending_merging_head[0] != InvalidBlockNumber to detect "another merger
       active". But:

       - pending_merging_head[0] is never reset to InvalidBlockNumber after merge (per §1.2). So after the first merge, no other
       merger ever runs.
       - 64 backends hitting threshold simultaneously all queue on metabuf EX. First one runs the (broken) merge, sets
       pending_merging_head[0] to non-Invalid. The other 63 each acquire meta EX, find it non-Invalid, and return. So at least the
       inline merge is single-flight. But it's single-flight forever.

       The unproductive-merge detection at :73-82 re-reads pending count and compares to pre-merge. With the current
       roaring_merge_pending resetting counts to 0, new_total < total_pending always, so merged_unproductively is never set, and
       after any merge the backend retries. Fine.

       But: the partitioned pending list with a single merger is not actually faster. The merger walks all 64 partitions
       sequentially in collect_pending (vacuum.c:46-82). 64× the page reads vs a single chain. Memory consumption: 64× the
       in-flight pages (mitigated by reading sequentially, releasing each buffer before the next).

       3.4 Unproductive merge detection

       Works at the global level (sum across partitions), but ignores the fact that if a single partition is hot and a single
       partition is empty, recomputing globally hides the imbalance. A smarter design merges only the partition over a
       per-partition threshold.

       ---
       4. Overflow Removal Review

       4.1 At 7500 bytes inline:

       A roaring32 bitmap container can be:
       - Array container: up to 4096 16-bit elements = 8192 bytes raw (plus ~10 bytes header). Above 4096 elements, converts to
       bitmap.
       - Bitmap container: fixed 8192 bytes (65,536 bits).
       - Run container: variable, 4 bytes per run.

       Plus a 24-byte serialized header per container, plus a top-level header that tracks N containers (8 bytes + 2 bytes per
       container for the high-bits + 8 bytes per container for offsets in portable format ≈ 18 bytes per container plus 8).

       A single dense bitmap container alone exceeds 7500 bytes. ANY key with TIDs spread over a 65k-range that's not
       run-compressible blows the limit. For the design's target workload of 6500 rows/value scattered across 6500 pages with 512
       offsets each (i.e. linearized TIDs spanning ~6500 × 512 = 3.3M), we're looking at potentially many bitmap-typed containers.
       Realistic: the inline limit is hit by any moderate-cardinality scattered workload.

       4.2 What happens when exceeded

       The build path's chunking attempts to keep each leaf entry ≤ 7500 bytes. But the search uses a wrong estimate ((mid -
       start_rank + 1) * 2 + 100) and validates with copy-then-measure, which is O(n) per step. Eventually for a key that cannot
       fit any multi-TID chunk in 7500 bytes (e.g. a single full bitmap container with 65k spread TIDs), the binary search
       converges to best_idx = start_rank (a 1-TID chunk), and the loop emits one entry per TID. For 6500 TIDs that's 6500 leaf
       entries × (8+4+4+1+3 + roaring(1 TID) ≈ 30 bytes) = 195 KB per key. Multiplied across 246K keys ≈ 48 GB. The entire space
       win of the AM is destroyed.

       For the merge path (unimplemented), there is no chunking at all, so the same data going through merge would just
       PageAddItem(8KB) → fail.

       4.3 Correct design

       Keep overflow pages, or rethink the chunking. Three options, ordered by my preference:

       A. Restore overflow pages. The original design was sound; the new "logical chunking" is a worse version of it. Overflow
       chains allow a single leaf entry to span pages; the leaf entry header sits inline (with a first_overflow_page BlockNumber),
       and the bitmap data flows through the chain. Cost: 1 extra page read per oversize key. Benefit: O(1) leaf entries per key,
       fits any roaring bitmap.

       B. Chunk on container boundaries. Roaring32 has a natural chunk structure: each container covers a 65k range identified by
       high-16-bits. Split into one leaf entry per high-16-bits prefix. Each entry holds a single container of at most 8 KB. With a
        header of ~20 bytes plus ~8 KB container, that won't fit in 7500. So adjust to ROARING_MAX_INLINE_BYTES = 8100 and require
       single-container chunks. Saves the chunking binary search. Still has the "millions of entries for a million-TID key" problem
        when scattered (16k chunks per million TIDs is bad).

       C. Hybrid: overflow pages for any container that doesn't fit inline. Container header inline, container body in overflow
       chain. Best of both. Closer to GIN's posting tree.

       My recommendation: option A. Restore overflow pages. Branch is reverting design progress.

       ---
       5. max_tid in Directory/Leaf

       5.1 Intent

       max_tid allows the directory binary search to break ties when multiple leaf entries share the same value (due to chunking).
       The scan code uses it (scan.c:50):

       if (entries[mid].high_key < value ||
           (entries[mid].high_key == value && entries[mid].max_tid < tid))

       So a scan can locate the chunk containing a specific TID. Useful for amgettuple, which this AM doesn't implement. For
       amgetbitmap, the scanner always wants ALL TIDs for a value — it never queries by specific TID — so tid is always passed as 0
        (scan.c:404, :470), and the binary search just finds the leftmost chunk containing high_key >= value. The max_tid field is
       unused in the bitmap path.

       5.2 Maintenance during merge

       There is no merge path, so this hasn't been thought through. But the relationship is:
       - Each leaf entry stores max_tid = max(linear_tid in chunk).
       - The dir entry stores max_tid = max_tid of the last entry on that leaf page (per build.c:332, :410, :439).

       When merge writes new chunks, both fields must be recomputed. With concurrent inserts going to the pending list (not into
       leaves), max_tid in leaves is stable between merges. OK.

       5.3 Concurrent insert interaction

       None — concurrent inserts go to pending. Merge holds locks to rewrite. Fine.

       5.4 Recommendation

       If you don't implement amgettuple, remove max_tid. It adds 4 bytes to every directory and leaf entry header (× 246K entries
       = ~1 MB) for no scan benefit. The directory comparator should fall back to "always return the leftmost leaf with high_key >=
        value" and the scanner walks right_page chains. Which is exactly what lookup_and_combine_value already does.

       If you DO want it (for future range pruning), document the invariant in the header struct comment, and add an assertion in
       build/merge that entry.max_tid == roaring_bitmap_maximum(bitmap).

       ---
       6. Performance Assessment

       6.1 Partitioned pending list — actual improvement

       At 8 concurrent writers: previously all 8 contended on the single metapage EX lock for the duration of an insert. With 64
       partitions, expected collisions on the same partition ≈ 8 × 7 / (2 × 64) ≈ 0.4 — so most inserts have no partition
       collision. Expected win: 5-8× throughput at 8 writers if the partitioning works correctly.

       At 32 writers: expected collisions ≈ 32 × 31 / 128 ≈ 7.75. Still better than fully serialized. Expected win: 4-6×.

       At 128 writers: more writers than partitions; each partition has ≈ 2 writers. Expected win: 2-3×.

       BUT: the actual code holds metabuf EX during normal append (insert.c:203). That serializes all 64 partitions on the metapage
        lock anyway. The lock is held only briefly (~tens of microseconds per insert), but at 1M inserts/sec it's the bottleneck
       again. Actual measured improvement vs main is probably 1.5-2× at 32 writers, far short of the 5-8× the design suggests.

       To get the partitioned win, the meta increment must not require metabuf EX. Options:
       - Use an atomic increment on a counter stored in shared memory (not on the metapage at all). Persist via WAL on merge.
       - Make the count approximate. Move the threshold check to a per-partition basis and read counters without locking (each
       backend uses pg_atomic_read_u32). Tolerate races; the merge re-checks anyway.

       6.2 Custom WAL — actual improvement

       See §2. ~30 bytes/insert saved. At 1M inserts/sec = 30 MB/s WAL. At the typical 100-300 KB/sec WAL on the old path, this
       drops WAL by 10-30%. Not transformative. Group commit (commit_delay) and parallel WAL writers will produce larger wins.

       6.3 Biggest remaining bottleneck

       After fixing the metabuf-EX-on-every-append issue (§6.1), the biggest bottleneck is MarkBufferDirty + WAL insert for every
       single row. GIN's fastupdate amortizes this by batching N inserts in user-space (via GinTupleCollector). pg_roaring_index
       inserts one entry per aminsert call, with one WAL record each. Even the custom 24-byte payload doesn't help when xlog header
        + 2 buffer descriptors dominate.

       Real fix: batch inserts in a per-backend memory buffer, flushed in aminsertcleanup (PG17+) or ExecutorEnd-style hook. One
       WAL record per ~500 inserts brings WAL down 50×.

       Secondary bottleneck: roaring_bitmap_portable_size_in_bytes in the build chunking loop is O(n) per call, called O(log n)
       times per chunk, per value. For high-cardinality builds, this dominates.

       ---
       7. Recommendations

       Must-fix before merge

       8. Implement roaring_merge_pending. Currently it discards all pending entries. Either revert to main's implementation and
       adapt to partitions, or implement the in-place chunked update described in CLAUDE.md. Without this, the branch loses data at
        the first merge.
       9. Implement ambulkdelete (and _lossy). Currently returns NULL — VACUUM cannot remove deleted TIDs from leaf bitmaps. Stale
       TIDs accumulate forever.
       10. Reset pending_merging_head[p] to InvalidBlockNumber at end of merge. Otherwise no future merge runs.
       11. Restore overflow pages. The 7500-byte cap silently mangles or refuses any moderate-cardinality key. Logical chunking is
       not a substitute.
       12. Fix the WAL redo function (§1.3, §1.4, §1.14):
         - Initialize buffer locals to InvalidBuffer.
         - Handle BLK_NOTFOUND / BLK_RESTORED properly.
         - Bounds-check entry_count before write.
         - Validate xlrec->partition.
         - Validate page type.
         - Don't apply meta increment if tail apply was skipped.
       13. Document and enforce a lock order. Currently: ambiguous. Suggest metabuf < tailbuf < newbuf < leaf < dir. Annotate every
       lock acquisition with a comment naming the order. Add an assertion (or LWLock tranche tracking) in debug builds.

       Should-fix before merge

       14. Drop the custom RMGR (or at least add rm_mask, rm_decode, and the safety checks in §2). The benefit is 30 bytes/insert;
       the cost is logical-decoding breakage, missing wal_consistency_checking, and a new failure surface. Use batched GenericXLog
       with multi-entry pending payloads instead.
       15. Replace MyProcPid % N with MyProcNumber % N. Predictable, dense distribution.
       16. Don't hold metabuf EX on the common-path append. Use atomic counters in shared memory, or a per-partition lightweight
       lock. Otherwise the partitioning is theatre.
       17. Remove max_tid if you're not implementing amgettuple — it costs 4 bytes per dir/leaf entry for zero scan benefit in the
       bitmap path.
       18. Batch inserts in aminsert + aminsertcleanup (PG17+). This is the actual ingest throughput win — far larger than WAL
       format optimization.

       Nice-to-have

       19. Add wal_consistency_checking masking even for the GenericXLog path (mask the [pd_lower, pd_upper) hole on pending pages,
        and the unused regions in metapage arrays beyond partitions_count).
       20. Add pg_stat_roaring / metapage inspection function so operators can see per-partition counts, find imbalance, and tune
       partitions.
       21. Drop the RoaringScanOpaque bitmap_loaded flag — it's a single bool; replace with scan->opaque == NULL check or inline
       into the scan state. Trivial cleanup.
       22. Audit roaring_build_lossy's linear_tid field — it stores just blkno (build.c:653, :680) but uses the same chunking that
       thinks in terms of dense TIDs. Verify that chunking still works for linear_tid ranges that are page-number-only (much
       narrower range, will compress to one container per key — chunking is mostly unused, but still need to verify the build-time
       binary-search doesn't misbehave).

       ---
       Bottom line

       The branch reflects an ambitious refactor where each individual change has merit (partition the hot path; reduce WAL size;
       simplify leaf layout) but the execution has unfinished surfaces (no merge, no bulkdelete, no overflow) and correctness
       defects (silent data loss in merge, WAL redo gaps, lock-order cycles). For a "millions of rows/hour" production workload, do
        not deploy this branch.

       The fastest path forward is to split it into 4 smaller PRs:
       23. Partitioned pending list, on top of main's existing merge + bulkdelete. Validate correctness, benchmark, decide on
       partitions default.
       24. Batched aminsert (per-backend buffer + cleanup callback). Bigger ingest win than custom WAL.
       25. (Optional) Custom WAL — only if benchmarks justify the maintenance cost, and only with rm_mask + rm_decode + the bounds
       checks.
       26. Logical chunking — if you keep it, on top of restored overflow pages, with a corrected size estimator.

       Key file paths for follow-up:
       - /Users/richardgilmore/pg_roaring_index/src/roaring_vacuum.c — broken merge, missing bulkdelete (single biggest gap)
       - /Users/richardgilmore/pg_roaring_index/src/roaring_wal.c — redo function defects
       - /Users/richardgilmore/pg_roaring_index/src/roaring_insert.c — lock order, metabuf EX hot path, TOCTOU re-check
       - /Users/richardgilmore/pg_roaring_index/src/roaring_build.c:241-384 — chunking binary search defects, oversize-key failure
       - /Users/richardgilmore/pg_roaring_index/include/pg_roaring_index.h:60 — ROARING_MAX_INLINE_BYTES = 7500 (too small for any
       single bitmap container)
       - /Users/richardgilmore/pg_roaring_index/src/pg_roaring_index.c:26-39 — _PG_init requires preload but no startup guard