#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    if let Err(error) = pi_cli::run_from_env().await {
        eprintln!("pi: {error}");
        std::process::exit(1);
    }
}
