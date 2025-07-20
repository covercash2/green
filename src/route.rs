//! Routes that are configured at runtime
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, derive_more::IntoIterator)]
pub struct Routes(HashMap<String, RouteInfo>);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteInfo {
    pub url: String,
    pub description: String,
}

