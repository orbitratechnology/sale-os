//! HwPrinter over TCP against the in-test emulator: every outcome rule that a network
//! printer can trigger, without hardware.

mod support;

use pos_device_hw::transport::TcpTransport;
use pos_device_hw::{HwPrinter, PrinterTimeouts, match_identified};
use pos_device_ports::{
    CashDrawerPort, DeviceError, DeviceHealth, DrawerPin, PaperLevel, PrintFailure, PrintResult,
    PrinterCapabilities, ReceiptOutputPort,
};
use std::sync::Arc;
use std::time::Duration;
use support::{Mode, Script, Status, TcpEmulator};

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

fn printer(port: u16) -> HwPrinter {
    HwPrinter::new(Arc::new(TcpTransport::new("127.0.0.1", port)), caps()).with_timeouts(quick())
}

async fn emulator(f: impl FnOnce(&mut Script)) -> TcpEmulator {
    let mut s = Script::default();
    f(&mut s);
    TcpEmulator::start(s).await
}

#[tokio::test]
async fn prints_and_reports_adapter_and_target() {
    let emu = emulator(|_| {}).await;
    let p = printer(emu.port());
    assert_eq!(p.adapter(), "escpos_net");
    assert_eq!(p.target(), format!("127.0.0.1:{}", emu.port()));
    assert_eq!(p.submit("R1", JOB).await, PrintResult::Printed);
    emu.wait_connections(1).await;
    assert_eq!(emu.all_job_bytes(), JOB);
    // Pre-flight 1, 2, 4 and the same after the job, on the one connection.
    let conns = emu.connections.lock().unwrap().clone();
    assert_eq!(conns.len(), 1);
    assert_eq!(conns[0].queries, [1, 2, 4, 1, 2, 4]);
}

#[tokio::test]
async fn blocking_states_fail_before_sending() {
    for (status, failure) in [
        (Status::PAPER_OUT, PrintFailure::PaperOut),
        (Status::COVER_OPEN, PrintFailure::CoverOpen),
        (Status::CUTTER_ERROR, PrintFailure::Offline),
    ] {
        let emu = emulator(|s| s.status = status).await;
        assert_eq!(
            printer(emu.port()).submit("R1", JOB).await,
            PrintResult::Failed(failure.clone())
        );
        emu.wait_connections(1).await;
        assert!(
            emu.all_job_bytes().is_empty(),
            "{failure:?}: no job byte may reach the printer"
        );
    }
}

#[tokio::test]
async fn offline_bit_alone_is_offline() {
    let emu = emulator(|s| {
        s.status = Status {
            printer: support::IDLE | support::OFFLINE_BIT,
            ..Status::READY
        }
    })
    .await;
    assert_eq!(
        printer(emu.port()).submit("R1", JOB).await,
        PrintResult::Failed(PrintFailure::Offline)
    );
}

#[tokio::test]
async fn nothing_listening_is_disconnected() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    assert_eq!(
        printer(port).submit("R1", JOB).await,
        PrintResult::Failed(PrintFailure::Disconnected)
    );
}

#[tokio::test]
async fn no_status_answer_still_prints() {
    let emu = emulator(|s| s.answers_status = false).await;
    assert_eq!(printer(emu.port()).submit("R1", JOB).await, PrintResult::Printed);
    emu.wait_connections(1).await;
    assert_eq!(emu.all_job_bytes(), JOB);
}

#[tokio::test]
async fn printer_that_never_reads_a_large_job_is_never_printed() {
    let emu = emulator(|s| s.mode = Mode::NeverRead).await;
    let p = printer(emu.port());
    let big = vec![b'x'; 64 * 1024 * 1024];
    // Bounded by the print queue's own timeout as well: either way, never Printed.
    let r = tokio::time::timeout(Duration::from_secs(10), p.submit("R1", &big)).await;
    match r {
        Ok(PrintResult::Uncertain(reason)) => assert!(reason.contains("timed out"), "{reason}"),
        Ok(other) => panic!("expected Uncertain, got {other:?}"),
        Err(_) => {} // the caller's timeout: unknown
    }
}

#[tokio::test]
async fn connection_dropped_mid_job_is_uncertain() {
    let emu = emulator(|s| s.mode = Mode::DropAfter(10_000)).await;
    let big = vec![b'y'; 16 * 1024 * 1024];
    match printer(emu.port()).submit("R1", &big).await {
        PrintResult::Uncertain(_) => {}
        other => panic!("expected Uncertain, got {other:?}"),
    }
}

#[tokio::test]
async fn paper_running_out_during_the_job_is_uncertain() {
    let emu = emulator(|s| s.after_job = Some(Status::PAPER_OUT)).await;
    match printer(emu.port()).submit("R1", JOB).await {
        PrintResult::Uncertain(reason) => assert!(reason.contains("paper out"), "{reason}"),
        other => panic!("expected Uncertain, got {other:?}"),
    }
}

#[tokio::test]
async fn printer_that_stops_answering_after_the_job_is_uncertain() {
    // 0x00 is not a valid status byte: the post-job query gets no usable answer.
    let silent = Status {
        printer: 0,
        offline: 0,
        error: 0,
        paper: 0,
    };
    let emu = emulator(|s| s.after_job = Some(silent)).await;
    match printer(emu.port()).submit("R1", JOB).await {
        PrintResult::Uncertain(reason) => assert!(reason.contains("stopped answering"), "{reason}"),
        other => panic!("expected Uncertain, got {other:?}"),
    }
}

#[tokio::test]
async fn status_and_health() {
    let emu = emulator(|s| s.status = Status::NEAR_END_DRAWER_HIGH).await;
    let p = printer(emu.port());
    let st = p.status().await.unwrap();
    assert!(st.online);
    assert_eq!(st.paper, PaperLevel::NearEnd);
    assert_eq!(st.drawer_open, Some(true));
    assert_eq!(p.is_open().await.unwrap(), Some(true));
    assert!(matches!(p.health().await, DeviceHealth::Warning(_)));

    emu.set(|s| s.status = Status::PAPER_OUT);
    assert_eq!(p.status().await.unwrap().paper, PaperLevel::Out);
    assert!(matches!(p.health().await, DeviceHealth::Offline(_)));

    let gone = printer(1);
    assert!(matches!(gone.status().await, Err(DeviceError::Disconnected(_))));
}

#[tokio::test]
async fn identify_via_gs_i() {
    let emu = emulator(|s| {
        s.maker = Some("EPSON".into());
        s.model = Some("TM-T88V".into());
        s.model_id = Some(0x20);
    })
    .await;
    let found = printer(emu.port()).identify().await.expect("identified");
    assert_eq!(found.maker.as_deref(), Some("EPSON"));
    assert_eq!(found.model.as_deref(), Some("TM-T88V"));
    assert_eq!(found.model_id, Some(0x20));
    assert_eq!(match_identified(&found).unwrap().id, "TM-T88V");

    let silent = emulator(|_| {}).await;
    assert_eq!(printer(silent.port()).identify().await, None);
}

#[tokio::test]
async fn drawer_kick_bytes() {
    let emu = emulator(|_| {}).await;
    let p = printer(emu.port());
    p.open(DrawerPin::Pin2).await.unwrap();
    p.open(DrawerPin::Pin5).await.unwrap();
    emu.wait_connections(2).await;
    assert_eq!(
        emu.jobs(),
        [vec![0x1B, 0x70, 0, 25, 250], vec![0x1B, 0x70, 1, 25, 250]]
    );

    let no_drawer = HwPrinter::new(
        Arc::new(TcpTransport::new("127.0.0.1", emu.port())),
        PrinterCapabilities::default(),
    );
    assert!(matches!(
        no_drawer.open(DrawerPin::Pin2).await,
        Err(DeviceError::Unsupported(_))
    ));
}

#[tokio::test]
async fn concurrent_jobs_are_serialised() {
    let emu = emulator(|s| s.chunk_delay = Duration::from_millis(20)).await;
    let p = Arc::new(printer(emu.port()));
    let mut tasks = Vec::new();
    for i in 0..6u8 {
        let p = Arc::clone(&p);
        tasks.push(tokio::spawn(async move {
            let job = vec![b'A' + i; 2000];
            p.submit(&format!("R{i}"), &job).await
        }));
    }
    for t in tasks {
        assert_eq!(t.await.unwrap(), PrintResult::Printed);
    }
    emu.wait_connections(6).await;
    assert_eq!(emu.max_active.load(std::sync::atomic::Ordering::SeqCst), 1);
    let jobs = emu.jobs();
    assert_eq!(jobs.len(), 6);
    for job in jobs {
        assert_eq!(job.len(), 2000);
        assert!(job.iter().all(|b| *b == job[0]), "jobs must not interleave");
    }
}
