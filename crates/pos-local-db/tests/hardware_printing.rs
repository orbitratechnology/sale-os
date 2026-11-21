//! E20-T08…T11 end to end: a real sale, printed through the real adapter (`pos-device-hw`,
//! raw TCP) to an in-test ESC/POS printer that behaves like an Epson TM-T88V: 42 columns,
//! its own code-page numbers, real-time status. Proves the bytes on the wire are what that
//! printer needs: accents and symbols in its code pages (no `?`), lines that fit 42
//! columns, the receipt number as a CODE128 barcode, a cut, and that a paper-out printer
//! fails the job before anything is sent.

mod common;

use pos_application::cart::CartService;
use pos_application::catalog::CatalogService;
use pos_application::checkout::{CheckoutService, CompleteSale};
use pos_application::printing::PrintService;
use pos_application::records::PrintStatus;
use pos_application::scan::ScanSource;
use pos_device_hw::{PrinterConfig, build_printer};
use pos_domain::catalog::{ProductInput, ProductType, Unit, VariantInput};
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::{Money, ScanEventId, UuidV7Generator};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A TCP printer: answers `DLE EOT n` with `status` (0x12 = ready), records job bytes.
struct Printer {
    port: u16,
    jobs: Arc<Mutex<Vec<Vec<u8>>>>,
    paper: Arc<AtomicU8>,
}

fn printer() -> Printer {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let jobs = Arc::new(Mutex::new(Vec::new()));
    let paper = Arc::new(AtomicU8::new(0x12));
    let (j, p) = (jobs.clone(), paper.clone());
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(mut c) = conn else { continue };
            c.set_read_timeout(Some(Duration::from_millis(300))).ok();
            let mut data = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                match c.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut i = 0;
                        while i < n {
                            if buf[i] == 0x10 && i + 2 < n && buf[i + 1] == 0x04 {
                                let reply = if buf[i + 2] == 4 {
                                    p.load(Ordering::SeqCst)
                                } else {
                                    0x12
                                };
                                // Paper out also shows as offline in DLE EOT 1.
                                let reply = if buf[i + 2] == 1 && p.load(Ordering::SeqCst) != 0x12 {
                                    0x1A
                                } else {
                                    reply
                                };
                                c.write_all(&[reply]).ok();
                                i += 3;
                            } else {
                                data.push(buf[i]);
                                i += 1;
                            }
                        }
                    }
                }
            }
            if !data.is_empty() {
                j.lock().unwrap().push(data);
            }
        }
    });
    Printer { port, jobs, paper }
}

/// One sale, printed to an in-test TM-T88V over TCP, and what it put on paper.
struct Printed {
    shop: common::Shop,
    p: Printer,
    prints: PrintService,
    bytes: Vec<u8>,
    /// The job decoded with the printer's own code pages, as the paper would show it.
    paper: String,
}

/// Sells one item called `name` and prints the receipt.
async fn sell_and_print(name: &str) -> Printed {
    let shop = common::shop().await;
    let s = shop.store.clone();
    let mut v = VariantInput::simple(Money::from_minor(1999));
    v.sku = Some("CAFE".into());
    CatalogService {
        store: Arc::new(s.clone()),
        catalog: Arc::new(s.clone()),
    }
    .create(
        &shop.owner,
        ProductInput {
            name: name.into(),
            description: None,
            category_id: None,
            product_type: ProductType::Stock,
            unit: Unit::Ea,
            tax_category_id: shop.tax_category,
            quick_select: false,
            variants: vec![v],
        },
    )
    .await
    .unwrap();
    let carts = Arc::new(CartService::new(
        Arc::new(s.clone()),
        Arc::new(s.clone()),
        Arc::new(s.clone()),
    ));
    carts
        .scan(
            &shop.owner,
            "CAFE",
            ScanEventId::generate(&UuidV7Generator),
            ScanSource::Manual,
        )
        .await
        .unwrap();
    let cart = carts.current(&shop.owner).await.unwrap().cart;
    CheckoutService {
        carts,
        store: Arc::new(s.clone()),
        setup: Arc::new(s.clone()),
        stock: Arc::new(s.clone()),
        receipts: Arc::new(s.clone()),
    }
    .complete_sale(
        &shop.owner,
        CompleteSale {
            cart_id: cart.id,
            revision: cart.revision,
            tenders: vec![TenderInput {
                kind: TenderKind::Cash,
                amount: Money::from_minor(2000),
                external_reference: None,
            }],
        },
    )
    .await
    .unwrap();

    let p = printer();
    let config: PrinterConfig = serde_json::from_value(serde_json::json!({
        "connection": { "kind": "network", "host": "127.0.0.1", "port": p.port },
        "profileId": "TM-T88V",
    }))
    .unwrap();
    let hw = build_printer(&config).unwrap();
    use pos_device_ports::ReceiptOutputPort;
    let caps = hw.capabilities();
    assert_eq!(caps.columns, Some(42));
    let prints = PrintService {
        store: Arc::new(s.clone()),
        prints: Arc::new(s.clone()),
        receipts: Arc::new(s.clone()),
        printer: hw,
        timeout: Duration::from_secs(10),
    };
    let job = prints.print_sale(&shop.owner, None).await.unwrap();
    assert_eq!(job.status, PrintStatus::Completed, "{job:?}");
    assert_eq!(job.adapter, "escpos_net");
    assert_eq!(
        job.target.as_deref(),
        Some(format!("127.0.0.1:{}", p.port).as_str())
    );

    let bytes = p.jobs.lock().unwrap().pop().expect("a job reached the printer");
    // Decode with the printer's own code-page numbers, as the paper would show it.
    let pages: Vec<(pos_receipts::CodePage, u8)> = caps
        .code_pages
        .iter()
        .filter_map(|(name, n)| pos_receipts::CodePage::from_name(name).map(|p| (p, *n)))
        .collect();
    let paper = pos_receipts::printer::decode(&bytes, pos_receipts::Dialect::EscPos, &pages);
    Printed {
        shop,
        p,
        prints,
        bytes,
        paper,
    }
}

#[tokio::test]
async fn a_real_sale_prints_correctly_on_a_42_column_epson_over_the_network() {
    let Printed {
        shop,
        p,
        prints,
        bytes,
        paper,
    } = sell_and_print("Café crème — Größe L Ąę £€").await;
    assert!(paper.contains("Café crème"), "{paper}");
    assert!(paper.contains("Größe"), "{paper}");
    assert!(paper.contains("Ąę"), "{paper}");
    assert!(paper.contains("£€"), "{paper}");
    assert!(!paper.contains('?'), "nothing lost: {paper}");
    assert!(paper.contains("BARCODE"), "receipt number as CODE128: {paper}");
    assert!(paper.contains("CUT"), "{paper}");
    // Every printed text line fits the TM-T88V's 42 columns.
    for op in pos_receipts::printer::parse(&bytes, pos_receipts::Dialect::EscPos) {
        if let pos_receipts::printer::Op::Text(t) = op {
            assert!(
                t.len() <= 42,
                "line wider than 42 columns: {:?}",
                String::from_utf8_lossy(&t)
            );
        }
    }

    // Paper out: refused before anything is sent, safe to retry, and the retry prints.
    p.paper.store(0x72, Ordering::SeqCst);
    let before = p.jobs.lock().unwrap().len();
    let failed = prints.print_sale(&shop.owner, None).await.unwrap();
    assert_eq!(failed.status, PrintStatus::Failed);
    assert_eq!(failed.last_error_code.as_deref(), Some("paper_out"));
    assert_eq!(p.jobs.lock().unwrap().len(), before, "no job bytes sent");
    p.paper.store(0x12, Ordering::SeqCst);
    let retried = prints.retry(&shop.owner, failed.job_id).await.unwrap();
    assert_eq!(retried.status, PrintStatus::Completed);
}

/// D-02: a Sinhala or Tamil name has no code page on any receipt printer. It reaches the
/// paper as a shaped image on the 42-column grid (504 dots), never as transliterated Latin,
/// and the rest of the receipt stays text.
#[tokio::test]
async fn sinhala_and_tamil_names_print_as_images_on_an_epson() {
    for name in ["පොල් තෙල් 750ml", "தேங்காய் எண்ணெய்"] {
        let Printed { bytes, paper, .. } = sell_and_print(name).await;
        assert!(paper.contains("[IMAGE 504x"), "{name}: {paper}");
        assert!(
            !paper.contains("pol") && !paper.contains("then"),
            "{name} transliterated: {paper}"
        );
        assert!(paper.contains("TOTAL"), "the rest stays text: {paper}");
        assert!(paper.contains("BARCODE") && paper.contains("CUT"), "{paper}");
        // Nothing in the image is read as a command (no stray cut or drawer kick).
        let ops = pos_receipts::printer::parse(&bytes, pos_receipts::Dialect::EscPos);
        let cuts = ops
            .iter()
            .filter(|op| matches!(op, pos_receipts::printer::Op::Command(c) if c.starts_with("CUT") || c.starts_with("DRAWER")))
            .count();
        assert_eq!(cuts, 1, "{paper}");
        assert!(
            !ops.iter()
                .any(|op| matches!(op, pos_receipts::printer::Op::Unknown(_))),
            "{paper}"
        );
    }
}

/// D-02: Arabic (with a Latin brand and digits), Devanagari and Chinese names reach the
/// paper as shaped images on the 42-column grid, not transliterated; the amounts and the
/// rest of the receipt stay text in the printer's code pages.
#[tokio::test]
async fn arabic_devanagari_and_chinese_names_print_as_images_on_an_epson() {
    for name in ["عصير Rani 250 مل", "मसाला चाय", "可口可乐 330ml"] {
        let Printed { bytes, paper, .. } = sell_and_print(name).await;
        assert!(paper.contains("[IMAGE 504x"), "{name}: {paper}");
        // Not transliterated, and the Arabic not sent as CP1256 text either (the TM-T88V
        // has that page, but it would print the letters unjoined and left to right).
        let transliterated = pos_receipts::fit_line(name, 42, &[]);
        assert!(
            !paper.contains(&transliterated[..5]),
            "{name} as {transliterated}: {paper}"
        );
        assert!(
            !paper.contains("Rani") && !paper.contains("330ml"),
            "{name} as text: {paper}"
        );
        assert!(paper.contains("19.99"), "the amounts stay text: {paper}");
        assert!(paper.contains("TOTAL"), "the rest stays text: {paper}");
        assert!(paper.contains("BARCODE") && paper.contains("CUT"), "{paper}");
        let ops = pos_receipts::printer::parse(&bytes, pos_receipts::Dialect::EscPos);
        assert!(
            !ops.iter()
                .any(|op| matches!(op, pos_receipts::printer::Op::Unknown(_))),
            "{paper}"
        );
    }
}
