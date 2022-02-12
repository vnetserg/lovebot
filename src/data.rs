use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

////////////////////////////////////////////////////////////////////////////////

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub login: String,
    pub first_name: String,
    pub last_name: Option<String>,
}

impl TryFrom<&teloxide::types::User> for User {
    type Error = anyhow::Error;

    fn try_from(tg_user: &teloxide::types::User) -> Result<Self> {
        Ok(User {
            login: tg_user
                .username
                .as_ref()
                .context("user has no username")?
                .clone(),
            first_name: tg_user.first_name.clone(),
            last_name: tg_user.last_name.clone(),
        })
    }
}

////////////////////////////////////////////////////////////////////////////////

pub type ThreadId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThreadAnonimityMode {
    Me,
    Them,
    Both,
}
