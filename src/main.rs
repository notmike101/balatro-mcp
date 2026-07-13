#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![allow(unexpected_cfgs)]

mod backend;
mod crash;
mod guide;
mod infra;
mod models;
mod protocol;
mod rules;
mod tools;

use backend::runtime;
use rmcp::ServiceExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::var_os("BALATRO_MCP_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let runtime_root = std::env::var_os("BALATRO_RUNTIME_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.clone());
    crash::install(runtime_root.join("agent").join("mcp_crash.log"));
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(value) = rules::cli(&root, &args)? {
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    if let Some(value) = runtime::cli(&runtime_root, &args)? {
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    let server = infra::Server::new(root).map_err(std::io::Error::other)?;
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
