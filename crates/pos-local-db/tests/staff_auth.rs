//! E05-T01 / E05-T02 acceptance: staff management and PIN sign-in against a real database.

use pos_application::LocalStore;
use pos_application::audit::{self, AuditEntry};
use pos_application::auth::{Argon2PinHasher, AuthService, PinHasher};
use pos_application::context::{Actor, AppError, CommandContext, RegisterContext};
use pos_application::queries::StaffQueries;
use pos_application::records::{BusinessRecord, LocationRecord, RegisterRecord, SessionEnd, StaffRecord};
use pos_application::staff::{NewStaff, StaffService};
use pos_application::{OutboxDraft, WriteSet};
use pos_domain::staff::{Pin, StaffError};
use pos_domain::{
    BusinessId, Clock, Currency, ErrorCode, HasErrorCode, LocationId, RegisterId, Role, SessionId, Timestamp,
    UserId, UuidV7Generator,
};
use pos_local_db::{DbOptions, LocalDb, SqliteStore};
use pos_testkit::FixedClock;
use std::sync::Arc;
use tempfile::TempDir;

struct World {
    _dir: TempDir,
    store: SqliteStore,
    clock: Arc<FixedClock>,
    register: RegisterContext,
    owner: StaffRecord,
    staff: StaffService,
    auth: AuthService,
}

fn pin(s: &str) -> Pin {
    Pin::parse(s).unwrap()
}

impl World {
    fn ctx(&self, actor: Option<&StaffRecord>) -> CommandContext {
        CommandContext {
            register: self.register.clone(),
            actor: actor.map(|s| Actor {
                user_id: s.user_id,
                role: s.role,
                session_id: SessionId::generate(&UuidV7Generator),
            }),
            clock: self.clock.clone(),
            ids: Arc::new(UuidV7Generator),
        }
    }
    fn as_owner(&self) -> CommandContext {
        self.ctx(Some(&self.owner))
    }
    async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(self.store.db().readers())
            .await
            .unwrap()
    }
}

/// Workspace + first owner, written directly (E04-T01's SetupService will do this).
async fn world() -> World {
    let dir = TempDir::new().unwrap();
    let store = SqliteStore::new(LocalDb::open(&DbOptions::in_dir(dir.path())).await.unwrap());
    let ids = UuidV7Generator;
    let clock = Arc::new(FixedClock::new(Timestamp::from_millis(1_767_225_600_000)));
    let t = Timestamp::from_millis(1_767_225_600_000);
    let register = RegisterContext {
        business_id: BusinessId::generate(&ids),
        location_id: LocationId::generate(&ids),
        register_id: RegisterId::generate(&ids),
    };
    // Cheap Argon2 parameters keep the suite fast; production parameters are timed separately.
    let hasher: Arc<dyn PinHasher> = Arc::new(Argon2PinHasher::with_params(256, 1, 1));
    let owner = StaffRecord {
        user_id: UserId::generate(&ids),
        business_id: register.business_id,
        display_name: "Olu".into(),
        role: Role::Owner,
        pin_hash: hasher.hash(&pin("1234")).unwrap(),
        pin_updated_at: t,
        must_change_pin: false,
        active: true,
        version: 1,
        created_at: t,
        updated_at: t,
    };
    let wctx = pos_application::WriteContext {
        business_id: register.business_id,
        location_id: register.location_id,
        register_id: register.register_id,
        actor: None,
        occurred_at: t,
    };
    let business = BusinessRecord {
        business_id: register.business_id,
        name: "Shop".into(),
        legal_name: None,
        tax_registration_no: None,
        address: serde_json::json!({}),
        phone: None,
        email: None,
        logo_path: None,
        country_code: "GB".into(),
        timezone: "Europe/London".into(),
        currency: Currency::from_iso("GBP").unwrap(),
        prices_include_tax: true,
        version: 1,
        created_at: t,
        updated_at: t,
    };
    let location = LocationRecord {
        location_id: register.location_id,
        business_id: register.business_id,
        name: "Main".into(),
        address: serde_json::json!({}),
        archived: false,
        version: 1,
        created_at: t,
        updated_at: t,
    };
    let reg = RegisterRecord {
        register_id: register.register_id,
        business_id: register.business_id,
        location_id: register.location_id,
        name: "Till".into(),
        code: "R01".into(),
        receipt_prefix: String::new(),
        retired: false,
        version: 1,
        created_at: t,
        updated_at: t,
    };
    let set = WriteSet::builder(wctx)
        .outbox(OutboxDraft::business_updated(&ids, &business).unwrap())
        .outbox(OutboxDraft::location_upserted(&ids, &location).unwrap())
        .outbox(OutboxDraft::register_upserted(&ids, &reg).unwrap())
        .outbox(OutboxDraft::staff_upserted(&ids, &owner).unwrap())
        .put_business(business, None)
        .put_location(location, None)
        .put_register(reg, None)
        .put_staff(owner.clone(), None)
        .audit(AuditEntry::new(
            &ids,
            audit::BUSINESS_CREATED,
            "business",
            register.business_id,
        ))
        .build()
        .unwrap();
    store.apply(set).await.unwrap();
    let s: Arc<dyn LocalStore> = Arc::new(store.clone());
    let q: Arc<dyn StaffQueries> = Arc::new(store.clone());
    World {
        _dir: dir,
        staff: StaffService {
            store: s.clone(),
            staff: q.clone(),
            hasher: hasher.clone(),
        },
        auth: AuthService {
            store: s,
            staff: q,
            hasher,
        },
        store,
        clock,
        register,
        owner,
    }
}

fn new_staff(name: &str, role: Role) -> NewStaff {
    NewStaff {
        display_name: name.into(),
        role,
        pin: pin("5678"),
    }
}

#[tokio::test]
async fn owner_manages_staff_and_others_cannot() {
    let w = world().await;
    let cashier = w
        .staff
        .create(&w.as_owner(), new_staff("Bea", Role::Cashier))
        .await
        .unwrap();
    let manager = w
        .staff
        .create(&w.as_owner(), new_staff("Cal", Role::Manager))
        .await
        .unwrap();
    assert!(
        cashier.pin_hash.starts_with("$argon2id$"),
        "PIN is stored as an Argon2id PHC string"
    );

    // IAM-002/003: only owners manage staff, checked in the application layer.
    for actor in [&cashier, &manager] {
        let err = w
            .staff
            .create(&w.ctx(Some(actor)), new_staff("Dee", Role::Cashier))
            .await
            .unwrap_err();
        assert_eq!(
            err,
            AppError::PermissionDenied(pos_domain::Permission::StaffManage)
        );
        assert_eq!(err.error_code(), ErrorCode::PermissionDenied);
    }
    let err = w
        .staff
        .create(&w.ctx(None), new_staff("Dee", Role::Cashier))
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::SessionLocked);

    // Every change synced with the PIN hash, never the PIN, and audited.
    let events = w
        .count("SELECT count(*) FROM outbox_events WHERE event_type = 'staff.upserted'")
        .await;
    assert_eq!(events, 3);
    let leaked = w
        .count("SELECT count(*) FROM outbox_events WHERE payload_json LIKE '%5678%'")
        .await;
    assert_eq!(leaked, 0, "the PIN itself must never be stored or synced");
    assert_eq!(
        w.count("SELECT count(*) FROM audit_events WHERE action = 'staff.created'")
            .await,
        2
    );
}

#[tokio::test]
async fn names_are_unique_among_active_staff_case_insensitively() {
    let w = world().await;
    let bea = w
        .staff
        .create(&w.as_owner(), new_staff("Bea", Role::Cashier))
        .await
        .unwrap();
    let err = w
        .staff
        .create(&w.as_owner(), new_staff(" bea ", Role::Cashier))
        .await
        .unwrap_err();
    assert_eq!(err, AppError::Staff(StaffError::NameTaken));
    // Once Bea is deactivated, the name is free again; her record is kept for history.
    w.staff.deactivate(&w.as_owner(), bea.user_id, 1).await.unwrap();
    w.staff
        .create(&w.as_owner(), new_staff("Bea", Role::Cashier))
        .await
        .unwrap();
    assert_eq!(
        w.count("SELECT count(*) FROM staff_users WHERE display_name = 'Bea'")
            .await,
        2
    );
}

#[tokio::test]
async fn the_last_owner_cannot_be_removed_or_demoted() {
    let w = world().await;
    let ctx = w.as_owner();
    let err = w.staff.deactivate(&ctx, w.owner.user_id, 1).await.unwrap_err();
    assert_eq!(err, AppError::Staff(StaffError::LastOwner));
    let err = w
        .staff
        .update(&ctx, w.owner.user_id, 1, None, Some(Role::Manager))
        .await
        .unwrap_err();
    assert_eq!(err, AppError::Staff(StaffError::LastOwner));
    // With a second owner, the first can step down.
    w.staff.create(&ctx, new_staff("Ola", Role::Owner)).await.unwrap();
    let demoted = w
        .staff
        .update(&ctx, w.owner.user_id, 1, None, Some(Role::Manager))
        .await
        .unwrap();
    assert_eq!((demoted.role, demoted.version), (Role::Manager, 2));
}

#[tokio::test]
async fn stale_edits_conflict() {
    let w = world().await;
    let bea = w
        .staff
        .create(&w.as_owner(), new_staff("Bea", Role::Cashier))
        .await
        .unwrap();
    w.staff
        .update(&w.as_owner(), bea.user_id, 1, Some("Beatrice".into()), None)
        .await
        .unwrap();
    let err = w
        .staff
        .update(&w.as_owner(), bea.user_id, 1, Some("Bee".into()), None)
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::ConflictVersion);
}

#[tokio::test]
async fn sign_in_with_pin_starts_a_session() {
    let w = world().await;
    let grant = w
        .auth
        .login(&w.ctx(None), w.owner.user_id, pin("1234"))
        .await
        .unwrap();
    assert_eq!((grant.role, grant.must_change_pin), (Role::Owner, false));
    assert_eq!(
        w.count("SELECT count(*) FROM staff_sessions WHERE ended_at IS NULL")
            .await,
        1
    );
    assert_eq!(
        w.count("SELECT count(*) FROM audit_events WHERE action = 'auth.login'")
            .await,
        1
    );

    let mut ctx = w.ctx(None);
    ctx.actor = Some(Actor {
        user_id: grant.user_id,
        role: grant.role,
        session_id: grant.session_id,
    });
    w.auth
        .end_session(&ctx, SessionEnd::InactivityLock)
        .await
        .unwrap();
    assert_eq!(
        w.count("SELECT count(*) FROM staff_sessions WHERE end_reason = 'inactivity_lock'")
            .await,
        1
    );
}

#[tokio::test]
async fn wrong_pins_lock_with_doubling_backoff_and_a_correct_pin_resets() {
    let w = world().await;
    let u = w.owner.user_id;
    for remaining in [4, 3, 2, 1] {
        let err = w.auth.login(&w.ctx(None), u, pin("0000")).await.unwrap_err();
        assert_eq!(
            err,
            AppError::PinInvalid {
                remaining_attempts: remaining
            }
        );
    }
    // 5th wrong PIN: locked for 30 s.
    let start = w.clock.now().as_millis();
    let err = w.auth.login(&w.ctx(None), u, pin("0000")).await.unwrap_err();
    assert_eq!(
        err,
        AppError::LockedOut {
            retry_at_ms: start + 30_000
        }
    );
    assert_eq!(err.error_code(), ErrorCode::AuthLockedOut);
    // Even the correct PIN is refused while locked.
    w.clock.advance_ms(29_000);
    assert!(matches!(
        w.auth.login(&w.ctx(None), u, pin("1234")).await,
        Err(AppError::LockedOut { .. })
    ));
    // After the lock, another wrong PIN locks for 60 s.
    w.clock.advance_ms(1_000);
    let now = w.clock.now().as_millis();
    assert_eq!(
        w.auth.login(&w.ctx(None), u, pin("0000")).await.unwrap_err(),
        AppError::LockedOut {
            retry_at_ms: now + 60_000
        }
    );
    // A correct PIN after that lock resets the counter.
    w.clock.advance_ms(60_000);
    w.auth.login(&w.ctx(None), u, pin("1234")).await.unwrap();
    assert_eq!(
        w.auth.login(&w.ctx(None), u, pin("0000")).await.unwrap_err(),
        AppError::PinInvalid {
            remaining_attempts: 4
        }
    );

    // Lockouts and failures are audited and sent to the cloud as audit.recorded.
    assert_eq!(
        w.count("SELECT count(*) FROM audit_events WHERE action = 'auth.locked_out'")
            .await,
        2
    );
    assert_eq!(
        w.count("SELECT count(*) FROM outbox_events WHERE event_type = 'audit.recorded'")
            .await,
        9
    );
}

#[tokio::test]
async fn reset_pin_forces_a_change_and_deactivated_staff_cannot_sign_in() {
    let w = world().await;
    let bea = w
        .staff
        .create(&w.as_owner(), new_staff("Bea", Role::Cashier))
        .await
        .unwrap();
    w.staff
        .reset_pin(&w.as_owner(), bea.user_id, 1, pin("9999"))
        .await
        .unwrap();
    let grant = w
        .auth
        .login(&w.ctx(None), bea.user_id, pin("9999"))
        .await
        .unwrap();
    assert!(grant.must_change_pin);

    let mut ctx = w.ctx(None);
    ctx.actor = Some(Actor {
        user_id: bea.user_id,
        role: Role::Cashier,
        session_id: grant.session_id,
    });
    assert_eq!(
        w.staff
            .change_own_pin(&ctx, pin("9999"), pin("9999"))
            .await
            .unwrap_err(),
        AppError::Staff(StaffError::SamePin)
    );
    assert!(matches!(
        w.staff.change_own_pin(&ctx, pin("1111"), pin("2468")).await,
        Err(AppError::PinInvalid { .. })
    ));
    w.staff
        .change_own_pin(&ctx, pin("9999"), pin("2468"))
        .await
        .unwrap();
    assert!(
        !w.auth
            .login(&w.ctx(None), bea.user_id, pin("2468"))
            .await
            .unwrap()
            .must_change_pin
    );

    let current = w.store.staff(bea.user_id).await.unwrap().unwrap();
    w.staff
        .deactivate(&w.as_owner(), bea.user_id, current.version)
        .await
        .unwrap();
    let err = w
        .auth
        .login(&w.ctx(None), bea.user_id, pin("2468"))
        .await
        .unwrap_err();
    assert_eq!(err, AppError::Staff(StaffError::Deactivated));
    assert!(
        w.auth
            .sign_in_list()
            .await
            .unwrap()
            .iter()
            .all(|s| s.user_id != bea.user_id)
    );
}
