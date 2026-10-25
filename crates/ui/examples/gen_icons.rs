//! Regenerates the committed hicolor icons: `cargo run -p fvp-ui --example gen_icons`.

use std::path::Path;

fn main() -> std::io::Result<()> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/icons");
    std::fs::create_dir_all(&dir)?;
    for n in [48u32, 128, 256] {
        let path = dir.join(format!("{n}.png"));
        std::fs::write(&path, fvp_ui::icon_png(n))?;
        println!("{}", path.display());
    }
    Ok(())
}
