use super::*;
use crate::bindings::{save_vendor_keys, save_vendor_oauth};
use crate::web_ui::routes::components::generate_openai_compatible_template;
use std::collections::HashMap;

mod capabilities;
mod passthrough;
mod rule_discovery;
mod signing;
mod storage;
