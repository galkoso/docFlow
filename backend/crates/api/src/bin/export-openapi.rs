use specspace_api::contract::ApiDoc;
use utoipa::OpenApi;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", ApiDoc::openapi().to_pretty_json()?);
    Ok(())
}
