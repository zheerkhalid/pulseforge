//! Build script: renders the PulseForge icon (vector, via tiny-skia) into a
//! multi-size .ico and a PNG for the window, then embeds the icon, manifest and version info.

#![allow(clippy::field_reassign_with_default)]

use std::{env, fs, path::PathBuf};
use tiny_skia::*;

fn rounded_rect(x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> Path {
    let mut pb = PathBuilder::new();
    pb.move_to(x0 + r, y0);
    pb.line_to(x1 - r, y0);
    pb.quad_to(x1, y0, x1, y0 + r);
    pb.line_to(x1, y1 - r);
    pb.quad_to(x1, y1, x1 - r, y1);
    pb.line_to(x0 + r, y1);
    pb.quad_to(x0, y1, x0, y1 - r);
    pb.line_to(x0, y0 + r);
    pb.quad_to(x0, y0, x0 + r, y0);
    pb.close();
    pb.finish().expect("rounded rect")
}

fn render_icon(size: u32) -> Vec<u8> {
    let mut pm = Pixmap::new(size, size).expect("pixmap");
    let s = size as f32 / 256.0;
    let id = Transform::identity();

    // Background: rounded square, hot pink -> purple gradient.
    let (x0, y0, x1, y1) = (8.0 * s, 8.0 * s, 248.0 * s, 248.0 * s);
    let bg = rounded_rect(x0, y0, x1, y1, 56.0 * s);
    let mut paint = Paint::default();
    paint.anti_alias = true;
    paint.shader = LinearGradient::new(
        Point::from_xy(x0, y0),
        Point::from_xy(x1, y1),
        vec![
            GradientStop::new(0.0, Color::from_rgba8(0xff, 0x3d, 0x8b, 255)),
            GradientStop::new(1.0, Color::from_rgba8(0x7a, 0x3c, 0xf0, 255)),
        ],
        SpreadMode::Pad,
        id,
    )
    .expect("gradient");
    pm.fill_path(&bg, &paint, FillRule::Winding, id, None);

    // A faint baseline behind the trace (dropped at tiny sizes).
    if size >= 40 {
        let mut pb = PathBuilder::new();
        pb.move_to(40.0 * s, 136.0 * s);
        pb.line_to(216.0 * s, 136.0 * s);
        let base = pb.finish().expect("baseline");
        let mut p = Paint::default();
        p.anti_alias = true;
        p.set_color_rgba8(255, 255, 255, 56);
        let stroke = Stroke {
            width: 6.0 * s,
            line_cap: LineCap::Round,
            ..Default::default()
        };
        pm.stroke_path(&base, &p, &stroke, id, None);
    }

    // The pulse: flat, small bump, tall spike, dip, flat.
    let pts: [(f32, f32); 8] = [
        (40.0, 136.0),
        (84.0, 136.0),
        (102.0, 104.0),
        (122.0, 190.0),
        (146.0, 58.0),
        (166.0, 160.0),
        (180.0, 136.0),
        (216.0, 136.0),
    ];
    let mut pb = PathBuilder::new();
    for (i, (x, y)) in pts.iter().enumerate() {
        if i == 0 {
            pb.move_to(x * s, y * s)
        } else {
            pb.line_to(x * s, y * s)
        }
    }
    let pulse = pb.finish().expect("pulse");
    let mut p = Paint::default();
    p.anti_alias = true;
    p.set_color_rgba8(255, 255, 255, 255);
    let stroke = Stroke {
        width: if size >= 32 { 17.0 * s } else { 24.0 * s },
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Default::default()
    };
    pm.stroke_path(&pulse, &p, &stroke, id, None);

    // The live dot at the end of the trace.
    if size >= 24 {
        let mut pb = PathBuilder::new();
        pb.push_circle(216.0 * s, 136.0 * s, 15.0 * s);
        let dot = pb.finish().expect("dot");
        pm.fill_path(&dot, &p, FillRule::Winding, id, None);
    }

    pm.encode_png().expect("png")
}

fn write_ico(path: &PathBuf, sizes: &[u32]) {
    let pngs: Vec<Vec<u8>> = sizes.iter().map(|&s| render_icon(s)).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (i, &s) in sizes.iter().enumerate() {
        let b = if s >= 256 { 0 } else { s as u8 };
        out.extend_from_slice(&[b, b, 0, 0, 1, 0, 32, 0]);
        out.extend_from_slice(&(pngs[i].len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += pngs[i].len() as u32;
    }
    for p in &pngs {
        out.extend_from_slice(p);
    }
    fs::write(path, out).expect("write ico");
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let ico = out.join("pulseforge.ico");
    write_ico(&ico, &[16, 20, 24, 32, 40, 48, 64, 256]);
    let png = render_icon(256);
    fs::write(out.join("icon-256.png"), &png).expect("write png");
    let docs_png = PathBuf::from("docs/assets/pulseforge-icon.png");
    if fs::read(&docs_png).ok().as_deref() != Some(&png[..]) {
        let _ = fs::create_dir_all("docs/assets");
        let _ = fs::write(&docs_png, &png);
    }

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let ver = env::var("CARGO_PKG_VERSION").expect("version");
    let parts: Vec<u16> = ver
        .split(['.', '-'])
        .take(3)
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    let (ma, mi, pa) = (
        parts[0],
        parts.get(1).copied().unwrap_or(0),
        parts.get(2).copied().unwrap_or(0),
    );
    let ver4 = format!("{ma}.{mi}.{pa}.0");
    println!("cargo:rerun-if-changed=res/pulseforge.manifest.in");
    let manifest = fs::read_to_string("res/pulseforge.manifest.in")
        .expect("manifest")
        .replace("@VERSION4@", &ver4);
    let manifest_path = out.join("pulseforge.manifest");
    fs::write(&manifest_path, manifest).expect("write manifest");
    let esc = |p: &PathBuf| p.display().to_string().replace('\\', "\\\\");
    let rc = format!(
        r#"#pragma code_page(65001)
1 ICON "{ico}"
1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {ma},{mi},{pa},0
PRODUCTVERSION {ma},{mi},{pa},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "Zheer Khalid (zheer.dev)"
      VALUE "FileDescription", "PulseForge hardware monitor"
      VALUE "FileVersion", "{ver}"
      VALUE "InternalName", "PulseForge"
      VALUE "LegalCopyright", "Copyright (c) 2026 Zheer Khalid. MIT License."
      VALUE "OriginalFilename", "PulseForge.exe"
      VALUE "ProductName", "PulseForge"
      VALUE "ProductVersion", "{ver}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        ico = esc(&ico),
        manifest = esc(&manifest_path),
    );
    let rc_path = out.join("pulseforge.rc");
    fs::write(&rc_path, rc).expect("write rc");
    embed_resource::compile(&rc_path, embed_resource::NONE);
    println!("cargo:rustc-link-arg-bins=/MANIFEST:NO");
}
