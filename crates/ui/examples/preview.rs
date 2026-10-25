//! Renders all preview PNGs: `cargo run -p fvp-ui --example preview [DIR]`
//! (default `target/ui-preview` under the workspace root).

use std::path::PathBuf;

fn main() -> std::io::Result<()> {
    let dir = std::env::args_os().nth(1).map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-preview"),
        PathBuf::from,
    );
    for path in fvp_ui::render_all_png(&dir)? {
        println!("{}", path.display());
    }
    Ok(())
}
