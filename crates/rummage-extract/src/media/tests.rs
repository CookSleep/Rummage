use std::fmt::Write;
use std::path::PathBuf;

use super::*;

/// 样本都很小：照片、页眉条、小图标、带透明的图、细长条、扫描页、1 秒的音视频
fn fixture(name: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("media")
        .join(name);
    std::fs::read(p).unwrap()
}

fn opts() -> MediaOpts {
    MediaOpts {
        images: true,
        audio: true,
        video: true,
        min_px: 128,
        max_count: 100,
        repeat_pages: 3,
        max_pixels: 100_000_000,
        max_entry_bytes: 1 << 20,
    }
}

/// 列出的媒体与取出的内容
fn render(name: &str, opts: &MediaOpts) -> String {
    let ext = name.rsplit_once('.').unwrap().1;
    let src = Source::open(ext, fixture(name)).unwrap().unwrap();
    let mut out = String::new();
    for e in src.list(opts).unwrap() {
        let loaded = match src.load(&e.at, opts).unwrap() {
            Loaded::Rgb {
                width,
                height,
                data,
            } => {
                assert_eq!(data.len(), (width * height * 3) as usize);
                format!("RGB {width}×{height}")
            }
            Loaded::File(b) => format!("文件 {} 字节", b.len()),
            Loaded::Range(a, b) => format!("范围 {} 字节", b - a),
        };
        writeln!(
            out,
            "{} 扫描页 {} → {}",
            serde_json::json!({ "embed": e.locator() }),
            e.scan,
            loaded
        )
        .unwrap();
    }
    out
}

#[test]
fn snapshot_formats() {
    for name in [
        "media.pdf",
        "media.pptx",
        "images.docx",
        "images.xlsx",
        "images.odt",
        "images.ods",
    ] {
        insta::assert_snapshot!(
            format!("media_{}", name.replace('.', "_")),
            render(name, &opts())
        );
    }
}

#[test]
fn limits_and_switches() {
    // 上限 1：扫描页不计入
    let one = MediaOpts {
        max_count: 1,
        ..opts()
    };
    insta::assert_snapshot!("media_pdf_max1", render("media.pdf", &one));
    // 重复 2 处即视为装饰：两张幻灯片上的同一张照片被跳过
    let twice = MediaOpts {
        repeat_pages: 2,
        ..opts()
    };
    insta::assert_snapshot!("media_pptx_repeat2", render("media.pptx", &twice));
    // 只处理音视频
    let media = MediaOpts {
        images: false,
        ..opts()
    };
    insta::assert_snapshot!("media_pptx_av_only", render("media.pptx", &media));
    assert!(render("media.pdf", &media).is_empty());
    // 条目超过解压上限时报错，不信任声明的大小
    let small = MediaOpts {
        max_entry_bytes: 100,
        ..opts()
    };
    let src = Source::open("docx", fixture("images.docx"))
        .unwrap()
        .unwrap();
    assert!(matches!(src.list(&small), Err(MediaError::TooLarge(_))));
    let err = src.load(&At::Part("word/media/image2.jpg".into()), &small);
    assert!(matches!(err, Err(MediaError::TooLarge(_))), "{err:?}");
}

/// JPEG 图像的字典把尺寸写小了（20×16）：按 JPEG 头部的 200×160 判断，像素上限也按它
#[test]
fn dct_uses_jpeg_size() {
    let src = Source::open("pdf", fixture("dct_small_dict.pdf"))
        .unwrap()
        .unwrap();
    let list = src.list(&opts()).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].size, Some((200, 160)));
    let tight = MediaOpts {
        max_pixels: 200 * 160 - 1,
        ..opts()
    };
    assert!(src.list(&tight).unwrap().is_empty());
    assert!(matches!(
        src.load(&list[0].at, &tight),
        Err(MediaError::TooLarge(_))
    ));
}

#[test]
fn locator_roundtrip() {
    let src = Source::open("pdf", fixture("media.pdf")).unwrap().unwrap();
    for e in src.list(&opts()).unwrap() {
        assert_eq!(Embedded::at_of(&e.locator()), Some(e.at.clone()));
    }
    assert_eq!(
        Embedded::at_of(
            &serde_json::json!({ "kind": "image", "n": 1, "part": "ppt/media/image1.jpg" })
        ),
        Some(At::Part("ppt/media/image1.jpg".into()))
    );
    // 带 SMask 的图：透明处合成为白色，不是黑色
    let at = At::Pdf { page: 2, pos: 3 };
    let Loaded::Rgb { data, .. } = src.load(&at, &opts()).unwrap() else {
        panic!("PDF 的图应解码为 RGB");
    };
    assert_eq!(data[..3], [255, 255, 255]);
    assert!(Source::open("md", Vec::new()).unwrap().is_none());
    assert!(Source::open("pdf", b"not a pdf".to_vec()).is_err());
}

#[test]
fn resolve_targets() {
    assert_eq!(
        resolve("ppt/slides/slide1.xml", "../media/image1.png").as_deref(),
        Some("ppt/media/image1.png")
    );
    assert_eq!(
        resolve("word/document.xml", "media/image1.png").as_deref(),
        Some("word/media/image1.png")
    );
    assert_eq!(
        resolve("ppt/presentation.xml", "/ppt/slides/slide2.xml").as_deref(),
        Some("ppt/slides/slide2.xml")
    );
    assert_eq!(resolve("word/document.xml", "../../x.png"), None);
    assert_eq!(
        resolve("word/document.xml", "https://example.com/a.png"),
        None
    );
}
