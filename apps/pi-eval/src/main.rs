#[tokio::main]
async fn main() {
    if let Err(error) = pi_eval_cli::run_from_env().await {
        eprintln!("pi-eval: {error}");
        std::process::exit(1);
    }
}
