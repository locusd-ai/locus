//! Built-in taggers — deterministic, no I/O, no LLM.

mod size;
mod convention;
mod topic;
mod complexity;

pub use size::SizeTagger;
pub use convention::ConventionTagger;
pub use topic::TopicTagger;
pub use complexity::ComplexityTagger;
