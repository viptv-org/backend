use super::*;

#[derive(Clone, Debug)]
pub enum Principal {
    Account {
        account_id: i64,
        role: String,
        profile_id: Option<i64>,
        session_id: Option<String>,
    },
}
impl Principal {
    pub fn key(&self) -> String {
        match self {
            Self::Account {
                account_id,
                profile_id,
                session_id,
                ..
            } => format!(
                "account:{account_id}:profile:{profile_id:?}:family:{}",
                session_id.as_deref().unwrap_or("unbound")
            ),
        }
    }
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Account { session_id, .. } => session_id.as_deref(),
        }
    }
    /// Revalidate the captured credential family and exact selected profile for media/resources.
    pub fn validate_scope(&self, db: &Connection) -> Result<(), ApiError> {
        let Self::Account {
            account_id,
            role,
            profile_id,
            session_id,
        } = self;
        let sid = session_id.as_deref().ok_or_else(unauthorized)?;
        let valid:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM auth_sessions s JOIN auth_accounts a ON a.id=s.account_id WHERE s.id=?1 AND s.account_id=?2 AND s.profile_id IS ?3 AND s.refresh_expires>?4 AND a.disabled=0 AND ((?5='device' AND s.kind='device') OR (?5=a.role AND s.kind='browser')))",params![sid,account_id,profile_id,now(),role],|r|r.get(0)).map_err(crate::db_error)?;
        if !valid {
            return Err(unauthorized());
        }
        if let Some(profile) = profile_id {
            self.require_profile(db, *profile)?;
        }
        Ok(())
    }
    pub fn can_profile(&self, db: &Connection, profile: i64) -> Result<bool, ApiError> {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM profile_owners o JOIN profiles p ON p.id=o.profile_id WHERE o.account_id=?1 AND o.profile_id=?2 AND p.presentation_complete=1)",
            params![self.account_id(), profile],
            |r| r.get(0),
        )
        .map_err(crate::db_error)
    }
    pub fn account_id(&self) -> Option<i64> {
        match self {
            Self::Account { account_id, .. } => Some(*account_id),
        }
    }
    pub fn is_owner(&self) -> bool {
        matches!(self, Self::Account {role,..} if role == "owner")
    }
    pub fn require_owner(&self) -> Result<(), ApiError> {
        if self.is_owner() {
            Ok(())
        } else {
            Err(forbidden())
        }
    }
    pub fn require_profile(&self, db: &Connection, profile: i64) -> Result<(), ApiError> {
        let ok = self.can_profile(db, profile)?;
        if ok {
            Ok(())
        } else {
            Err(forbidden())
        }
    }
}
