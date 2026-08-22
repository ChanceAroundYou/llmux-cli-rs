pub mod aliases;
pub mod available;
pub mod health;
pub mod stream;
pub mod testing;

use serde::{Deserialize, Serialize};

pub use aliases::{delete_model_alias, get_model_aliases, set_model_alias};
pub use available::{fetch_provider_models, get_available_models};
pub use stream::stream_available_models;
pub use health::get_models_health;
pub use testing::{get_test_queue_status, start_test_queue, test_model};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TestQueueState {
    pub is_running: bool,
    pub total: usize,
    pub current: usize,
    pub progress: usize,
}
