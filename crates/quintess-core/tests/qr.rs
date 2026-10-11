//! QR codes printed on pages are found, read and placed.

use quintess_core::create::{TextPageSettings, text_to_pdf};
use quintess_core::qr::{CodeKind, find_in_document};
use quintess_core::stamp::{Anchor, ImageStamp, stamp_image};
use quintess_core::{SaveSettings, engine};

fn code_image(text: &str, name: &str) -> std::path::PathBuf {
    let path = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    qrcode::QrCode::new(text.as_bytes()).unwrap().render::<image::Luma<u8>>().quiet_zone(true).build().save(&path).unwrap();
    path
}

#[test]
fn codes_on_pages_are_read_and_located() {
    let mut pdf = text_to_pdf("Scan to read the menu.\n\n\n\nGuest network below.", &TextPageSettings::default()).unwrap();
    let menu = code_image("https://example.com/menu?table=12", "qr-menu.png");
    let wifi = code_image("WIFI:T:WPA;S:Cafe Guest;P:latte-art-42;;", "qr-wifi.png");
    // A large code at the top right and a small one (half an inch) at the bottom left.
    stamp_image(&mut pdf, &[1], &menu, &ImageStamp { width: 120.0, anchor: Anchor::TopRight, margin: 48.0, opacity: 1.0, behind_content: false }).unwrap();
    stamp_image(&mut pdf, &[1], &wifi, &ImageStamp { width: 48.0, anchor: Anchor::BottomLeft, margin: 48.0, opacity: 1.0, behind_content: false }).unwrap();
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let document = engine::open_document(bytes, None).unwrap();
    let mut found = find_in_document(&document, &|_, _| {}).unwrap();
    found.sort_by(|a, b| a.display_rect[1].total_cmp(&b.display_rect[1]));
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].kind, CodeKind::WebAddress { url: "https://example.com/menu?table=12".into() });
    // Top right of a letter page: the code sits within the 120-point stamp.
    let [left, top, right, _] = found[0].display_rect;
    assert!(left > 612.0 - 48.0 - 125.0 && right < 612.0 - 40.0 && top < 48.0 + 30.0, "{:?}", found[0].display_rect);
    assert_eq!(found[1].kind, CodeKind::Wifi { network: "Cafe Guest".into(), security: "WPA".into(), password: "latte-art-42".into(), hidden: false });
}
