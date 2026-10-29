use super::*;
#[derive(Clone)]
pub(crate) struct ParentScope {
    profile: i64,
    lease: ResourceLease,
    policy: i64,
    configuration: u64,
}
impl ParentScope {
    pub(crate) fn new(app: &App, profile: i64) -> Result<Self, ApiError> {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        let lease = app.request_lease();
        lease.validate(&db)?;
        manager(&db, &lease.principal, Some(profile))?;
        let policy = db
            .query_row(
                "SELECT revision FROM kids_profiles WHERE profile_id=?1",
                [profile],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_error)?
            .unwrap_or(0);
        let configuration = db
            .query_row(
                "SELECT revision FROM engine_profile_configuration WHERE profile_id=?1",
                [profile],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_error)?
            .unwrap_or(0);
        Ok(Self {
            profile,
            lease,
            policy,
            configuration,
        })
    }
    pub(crate) fn profile(&self) -> i64 {
        self.profile
    }
    pub(crate) fn lease(&self) -> &ResourceLease {
        &self.lease
    }
    pub(crate) fn validate(&self, db: &Connection) -> Result<(), ApiError> {
        self.lease.validate(db)?;
        manager(db, &self.lease.principal, Some(self.profile))?;
        let policy: i64 = db
            .query_row(
                "SELECT revision FROM kids_profiles WHERE profile_id=?1",
                [self.profile],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_error)?
            .unwrap_or(0);
        let configuration: u64 = db
            .query_row(
                "SELECT revision FROM engine_profile_configuration WHERE profile_id=?1",
                [self.profile],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_error)?
            .unwrap_or(0);
        if policy != self.policy {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "Target profile policy changed".into(),
            ));
        }
        if configuration != self.configuration {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "Target profile configuration changed".into(),
            ));
        }
        Ok(())
    }
    pub(crate) fn recheck(&self, app: &App) -> Result<(), ApiError> {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        self.validate(&db)
    }
}
