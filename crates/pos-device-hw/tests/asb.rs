//! Automatic status back (ESC/POS `GS a`): the printer pushes its state while idle, the
//! till shows it at once, printers that never push fall back to polling, and ASB blocks
//! that arrive between `DLE EOT` replies are told apart. Against the in-test TCP printer and,
//! on unix, a PTY pair.

mod support;

use pos_device_hw::transport::TcpTransport;
use pos_device_hw::{AsbTimings, HwPrinter, PrinterTimeouts, StatusSource};
use pos_device_ports::{
    DeviceHealth, PaperLevel, PrintFailure, PrintResult, PrinterCapabilities, PushedStatus, ReceiptOutputPort,
};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use support::{Script, Status, TcpEmulator};
use tokio::sync::watch;

const JOB: &[u8] = b"HELLO\nTOTAL 9.99\n\x1d\x56\x01";

fn caps() -> PrinterCapabilities {
    PrinterCapabilities {
        status_query: true,
        drawer_kick: true,
        ..PrinterCapabilities::default()
    }
}

fn quick() -> PrinterTimeouts {
    PrinterTimeouts {
        connect: Duration::from_millis(500),
        write: Duration::from_millis(1500),
        status: Duration::from_millis(300),
        post_status: Duration::from_millis(800),
        drain: Duration::from_millis(500),
        spool_confirm: Duration::from_millis(500),
        job: Duration::from_secs(5),
    }
}

fn asb(liveness: Duration) -> AsbTimings {
    AsbTimings {
        first: Duration::from_millis(300),
        liveness,
        slice: Duration::from_millis(20),
        retry_max: Duration::from_millis(300),
    }
}

fn tcp_printer(port: u16, liveness: Duration) -> Arc<HwPrinter> {
    Arc::new(
        HwPrinter::new(Arc::new(TcpTransport::new("127.0.0.1", port)), caps())
            .with_timeouts(quick())
            .with_asb_timings(asb(liveness)),
    )
}

async fn emulator(f: impl FnOnce(&mut Script)) -> TcpEmulator {
    let mut s = Script::default();
    f(&mut s);
    TcpEmulator::start(s).await
}

async fn wait_source(p: &HwPrinter, want: StatusSource) {
    let start = Instant::now();
    while p.status_source() != want {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "still {:?}, wanted {want:?}",
            p.status_source()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Waits for a pushed status that satisfies `f`; returns how long it took.
async fn wait_pushed(rx: &mut watch::Receiver<PushedStatus>, f: impl Fn(&PushedStatus) -> bool) -> Duration {
    let start = Instant::now();
    tokio::time::timeout(Duration::from_secs(5), rx.wait_for(|s| f(s)))
        .await
        .expect("no such status pushed in time")
        .expect("status channel closed");
    start.elapsed()
}

fn paper(s: &PushedStatus) -> Option<PaperLevel> {
    match s {
        PushedStatus::Status(st) => Some(st.paper),
        _ => None,
    }
}

#[tokio::test]
async fn asb_is_switched_on_when_the_status_link_opens() {
    let emu = emulator(|s| s.asb = true).await;
    let p = tcp_printer(emu.port(), Duration::from_secs(60));
    assert_eq!(p.status_source(), StatusSource::Probing);
    p.start_status_watch();
    wait_source(&p, StatusSource::Pushed).await;
    assert!(p.pushes_status());
    assert_eq!(*emu.asb_commands.lock().unwrap(), [0x0F], "GS a 15: everything");

    // The state is known without asking: no new connection, no DLE EOT.
    let accepted = emu.accepted.load(Ordering::SeqCst);
    let st = p.status().await.unwrap();
    assert_eq!(st.paper, PaperLevel::Ok);
    assert!(st.online && !st.cover_open);
    assert_eq!(p.health().await, DeviceHealth::Ready);
    assert_eq!(emu.accepted.load(Ordering::SeqCst), accepted);
    assert_eq!(emu.active(), 1, "the status link stays open while idle");
}

#[tokio::test]
async fn paper_out_pushed_while_idle_shows_at_once() {
    let emu = emulator(|s| s.asb = true).await;
    let p = tcp_printer(emu.port(), Duration::from_secs(60));
    let mut rx = p.status_updates().expect("an ESC/POS printer on TCP can push");
    p.start_status_watch();
    wait_pushed(&mut rx, |s| paper(s) == Some(PaperLevel::Ok)).await;

    emu.push(Status::PAPER_OUT);
    let took = wait_pushed(&mut rx, |s| paper(s) == Some(PaperLevel::Out)).await;
    assert!(took < Duration::from_millis(500), "took {took:?}");
    assert_eq!(p.health().await, DeviceHealth::Offline("out of paper".into()));
    assert_eq!(
        p.status().await.unwrap().blocking_failure(),
        Some(PrintFailure::PaperOut)
    );

    emu.push(Status::COVER_OPEN);
    wait_pushed(
        &mut rx,
        |s| matches!(s, PushedStatus::Status(st) if st.cover_open),
    )
    .await;
    emu.push(Status::READY);
    wait_pushed(
        &mut rx,
        |s| matches!(s, PushedStatus::Status(st) if st.blocking_failure().is_none()),
    )
    .await;
    assert_eq!(p.health().await, DeviceHealth::Ready);
}

#[tokio::test]
async fn a_printer_that_ignores_asb_falls_back_to_polling() {
    let emu = emulator(|_| {}).await;
    let p = tcp_printer(emu.port(), Duration::from_secs(60));
    let rx = p.status_updates().unwrap();
    p.start_status_watch();
    wait_source(&p, StatusSource::Polled).await;
    assert!(!p.pushes_status());
    assert_eq!(*rx.borrow(), PushedStatus::None);
    // Asked twice (two links), then left alone.
    assert_eq!(*emu.asb_commands.lock().unwrap(), [0x0F, 0x0F]);

    // Polling still works: the state comes from DLE EOT.
    emu.push(Status::NEAR_END_DRAWER_HIGH);
    assert_eq!(
        p.health().await,
        DeviceHealth::Warning("paper running low".into())
    );
    emu.wait_connections(3).await;
    let conns = emu.connections.lock().unwrap().clone();
    assert_eq!(conns.last().unwrap().queries, [1, 2, 4]);
    assert_eq!(p.submit("R1", JOB).await, PrintResult::Printed);
}

#[tokio::test]
async fn a_silent_printer_is_polled_and_still_prints() {
    // Answers neither ASB nor DLE EOT (E20-T08): status unknown, every job printed, and the
    // silent-printer rule (stop asking after two jobs) unchanged.
    let emu = emulator(|s| s.answers_status = false).await;
    let p = tcp_printer(emu.port(), Duration::from_secs(60));
    p.start_status_watch();
    wait_source(&p, StatusSource::Polled).await;
    for n in 0..4 {
        assert_eq!(p.submit(&format!("R{n}"), JOB).await, PrintResult::Printed);
    }
    emu.wait_connections(6).await;
    let conns = emu.connections.lock().unwrap().clone();
    let asked: Vec<_> = conns
        .iter()
        .filter(|c| !c.job.is_empty())
        .map(|c| c.queries.len())
        .collect();
    assert_eq!(asked, [1, 1, 0, 0]);
    assert_eq!(p.status().await.unwrap().paper, PaperLevel::Unknown);
}

#[tokio::test]
async fn asb_blocks_inside_dle_eot_replies_are_told_apart() {
    // Every reply arrives in the middle of an ASB block. Taking the block's first byte for
    // the reply would read "no answer" and print into an empty printer.
    let emu = emulator(|s| {
        s.asb_noise = true;
        s.status = Status::PAPER_OUT;
    })
    .await;
    let p = tcp_printer(emu.port(), Duration::from_secs(60));
    let mut rx = p.status_updates().unwrap();
    assert_eq!(
        p.submit("R1", JOB).await,
        PrintResult::Failed(PrintFailure::PaperOut)
    );
    emu.wait_connections(1).await;
    assert!(emu.all_job_bytes().is_empty(), "nothing sent");
    // The blocks were not lost either: they say the same.
    wait_pushed(&mut rx, |s| paper(s) == Some(PaperLevel::Out)).await;

    emu.push(Status::READY);
    assert_eq!(p.submit("R2", JOB).await, PrintResult::Printed);
    emu.wait_connections(2).await;
    let conns = emu.connections.lock().unwrap().clone();
    assert_eq!(conns[1].queries, [1, 2, 4, 1, 2, 4], "both checks answered");
    assert_eq!(emu.all_job_bytes(), JOB);
    wait_pushed(&mut rx, |s| paper(s) == Some(PaperLevel::Ok)).await;
}

#[tokio::test]
async fn a_job_takes_the_status_link_over_and_gives_it_back() {
    let emu = emulator(|s| s.asb = true).await;
    let p = tcp_printer(emu.port(), Duration::from_secs(60));
    let mut rx = p.status_updates().unwrap();
    p.start_status_watch();
    wait_source(&p, StatusSource::Pushed).await;

    let started = Instant::now();
    assert_eq!(p.submit("R1", JOB).await, PrintResult::Printed);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(emu.all_job_bytes(), JOB);
    // One connection at a time: the status link closed before the job connected.
    assert_eq!(emu.max_active.load(Ordering::SeqCst), 1);

    // The status link comes back and switches ASB on again (a receipt's ESC @ may reset it).
    let start = Instant::now();
    while emu.asb_commands.lock().unwrap().len() < 2 {
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "status link not reopened"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    emu.push(Status::PAPER_OUT);
    wait_pushed(&mut rx, |s| paper(s) == Some(PaperLevel::Out)).await;
    assert_eq!(
        p.submit("R2", JOB).await,
        PrintResult::Failed(PrintFailure::PaperOut)
    );
}

#[tokio::test]
async fn the_liveness_check_notices_a_printer_that_went_quiet() {
    let emu = emulator(|s| s.asb = true).await;
    let p = tcp_printer(emu.port(), Duration::from_millis(300));
    let mut rx = p.status_updates().unwrap();
    p.start_status_watch();
    wait_source(&p, StatusSource::Pushed).await;

    // Hung: the connection stays up but nothing comes back any more.
    emu.set(|s| s.asb = false);
    wait_pushed(&mut rx, |s| matches!(s, PushedStatus::Lost(_))).await;
    assert!(matches!(p.health().await, DeviceHealth::Offline(_)));
    assert!(p.pushes_status(), "still expected to push: it did before");

    // Back: the next attempt hears it again.
    emu.set(|s| s.asb = true);
    wait_pushed(&mut rx, |s| paper(s) == Some(PaperLevel::Ok)).await;
    assert_eq!(p.health().await, DeviceHealth::Ready);
}

#[tokio::test]
async fn the_spooler_and_status_less_profiles_never_push() {
    let emu = emulator(|s| s.asb = true).await;
    let p = Arc::new(
        HwPrinter::new(
            Arc::new(TcpTransport::new("127.0.0.1", emu.port())),
            PrinterCapabilities::default(),
        )
        .with_timeouts(quick()),
    );
    assert!(p.status_updates().is_none());
    p.start_status_watch();
    assert_eq!(p.status_source(), StatusSource::Polled);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(emu.accepted.load(Ordering::SeqCst), 0, "never connected");
}

#[cfg(unix)]
mod serial {
    use super::*;
    use pos_device_hw::config::SerialSettings;
    use pos_device_hw::transport::SerialTransport;
    use support::SerialEmulator;

    fn serial_printer(path: &str) -> Arc<HwPrinter> {
        Arc::new(
            HwPrinter::new(
                Arc::new(SerialTransport::new(SerialSettings::new(path, 19200))),
                caps(),
            )
            .with_timeouts(PrinterTimeouts {
                drain: Duration::from_millis(100),
                ..quick()
            })
            .with_asb_timings(asb(Duration::from_secs(60))),
        )
    }

    #[tokio::test]
    async fn serial_paper_out_pushed_mid_idle_and_jobs_still_print() {
        let emu = SerialEmulator::start(Script {
            asb: true,
            ..Script::default()
        });
        let p = serial_printer(&emu.slave_path);
        let mut rx = p.status_updates().unwrap();
        p.start_status_watch();
        wait_source(&p, StatusSource::Pushed).await;
        assert_eq!(emu.asb_commands(), [0x0F]);

        emu.push(Status::PAPER_OUT);
        let took = wait_pushed(&mut rx, |s| paper(s) == Some(PaperLevel::Out)).await;
        assert!(took < Duration::from_millis(500), "took {took:?}");
        assert_eq!(p.health().await, DeviceHealth::Offline("out of paper".into()));

        emu.push(Status::READY);
        wait_pushed(&mut rx, |s| paper(s) == Some(PaperLevel::Ok)).await;
        // The port is exclusive: the job gets it from the status link, then hands it back.
        assert_eq!(p.submit("R1", b"SERIAL RECEIPT\n").await, PrintResult::Printed);
        assert_eq!(emu.wait_job(15), b"SERIAL RECEIPT\n");
        let start = Instant::now();
        while emu.asb_commands().len() < 2 {
            assert!(
                start.elapsed() < Duration::from_secs(3),
                "status link not reopened"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        emu.push(Status::COVER_OPEN);
        wait_pushed(
            &mut rx,
            |s| matches!(s, PushedStatus::Status(st) if st.cover_open),
        )
        .await;
    }

    #[tokio::test]
    async fn a_serial_printer_without_asb_is_polled() {
        let emu = SerialEmulator::start(Script::default());
        let p = serial_printer(&emu.slave_path);
        p.start_status_watch();
        wait_source(&p, StatusSource::Polled).await;
        emu.push(Status::PAPER_OUT);
        assert_eq!(p.health().await, DeviceHealth::Offline("out of paper".into()));
    }
}
