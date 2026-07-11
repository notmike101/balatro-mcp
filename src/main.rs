mod models;
mod infra;
mod protocol;
mod guide;
mod tools;

use rmcp::ServiceExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::var_os("BALATRO_MCP_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let server = infra::Server::new(root).map_err(std::io::Error::other)?;
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
