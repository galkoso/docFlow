use specspace_infrastructure::MIGRATOR;
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database_url = std::env::var("DATABASE_URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(10))
        .connect(&database_url)
        .await?;
    MIGRATOR.run(&pool).await?;
    println!("PostgreSQL migrations applied.");
    pool.close().await;
    Ok(())
}
