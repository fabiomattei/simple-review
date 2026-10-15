//! Create the fixture repository on disk: `cargo run -p review-core --example make_fixture -- <dir>`.

fn main() -> anyhow::Result<()> {
    let dir = std::env::args().nth(1).ok_or_else(|| anyhow::anyhow!("usage: make_fixture <empty-dir>"))?;
    let fixture = review_core::fixture::build(std::path::Path::new(&dir))?;
    for (name, id) in &fixture.commits {
        println!("{name:12} {id}");
    }
    Ok(())
}
