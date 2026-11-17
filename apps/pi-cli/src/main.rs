#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    match pi_cli::run_from_env_with_exit_code().await {
        Ok(0) => {}
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("pi: {error}");
            std::process::exit(1);
        }
    }
}
