//! Owner accounts (E16-T03): a global table, read and written only by the authentication
//! code. Runs as `pos_app` like everything else; the table has no tenant.

use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    #[error("an account with this email already exists")]
    EmailTaken,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// What sign-in needs: the account, its password hash and whether it may sign in.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct AccountRow {
    pub account_id: Uuid,
    pub password_hash: Option<String>,
    pub status: String,
}

/// Creates an account. The email is compared without case (`citext`).
pub async fn create(
    pool: &PgPool,
    account_id: Uuid,
    email: &str,
    password_hash: &str,
) -> Result<(), AccountError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL ROLE pos_app").execute(&mut *tx).await?;
    let inserted = sqlx::query(
        "INSERT INTO accounts (account_id, email, password_hash) VALUES ($1, $2, $3) ON CONFLICT (email) DO NOTHING",
    )
    .bind(account_id)
    .bind(email)
    .bind(password_hash)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if inserted == 0 {
        return Err(AccountError::EmailTaken);
    }
    tx.commit().await?;
    Ok(())
}

pub async fn by_email(pool: &PgPool, email: &str) -> Result<Option<AccountRow>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL ROLE pos_app").execute(&mut *tx).await?;
    let row = sqlx::query_as("SELECT account_id, password_hash, status FROM accounts WHERE email = $1")
        .bind(email)
        .fetch_optional(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(row)
}
