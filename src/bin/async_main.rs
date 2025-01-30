#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]

use embassy_executor::Spawner;
use embassy_sync::pipe;
use esb::{self};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::sync::RawMutex;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::timer::AnyTimer;
use esp_hal::uart::Uart;
use esp_hal_embassy as _;
use esp_hal_embassy::InterruptExecutor;
use static_cell::StaticCell;

extern crate alloc;

#[esp_hal_embassy::main]
async fn main(low_prio_spawner: Spawner) {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(72 * 1024);

    esp_println::logger::init_logger_from_env();

    let timg1 = TimerGroup::new(peripherals.TIMG1);
    let timer: AnyTimer = timg1.timer0.into();

    esp_hal_embassy::init(timer);

    let sw_ints =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);

    static EXECUTOR: StaticCell<InterruptExecutor<0>> = StaticCell::new();
    let executor = InterruptExecutor::new(sw_ints.software_interrupt0);
    let executor = EXECUTOR.init(executor);

    let prio_spawner = executor.start(esp_hal::interrupt::Priority::Priority3);

    let rng = esp_hal::rng::Rng::new(peripherals.RNG);

    log::info!("Embassy initialized!");

    let (wifi_controller, wifi_stack, wifi_runner) = esb::wifi::init(
        peripherals.TIMG0,
        peripherals.RADIO_CLK,
        peripherals.WIFI,
        rng.clone(),
    );

    low_prio_spawner
        .spawn(esb::wifi::connection(wifi_controller))
        .ok();
    low_prio_spawner
        .spawn(esb::wifi::net_task(wifi_runner))
        .ok();

    let (uart_rx, uart_tx) = Uart::new(
        peripherals.UART1,
        esp_hal::uart::Config::default()
            .with_rx_fifo_full_threshold(100)
            .with_rx_timeout(1),
    )
    .unwrap()
    .with_rx(peripherals.GPIO2)
    .with_tx(peripherals.GPIO3)
    .into_async()
    .split();

    static mut PIPE: pipe::Pipe<RawMutex, { esb::uart::PIPE_SIZE }> = pipe::Pipe::new();
    let (pipe_reader, pipe_writer) = unsafe { PIPE.split() };

    // Read from UART and write to pipe
    let uart_reader = esb::uart::UartReader::new(uart_rx, pipe_writer);
    prio_spawner.spawn(esb::uart::task(uart_reader)).ok();

    // Dev: Send test data to UART
    #[cfg(feature = "dev-uart")]
    {
        let uart_writer = esb::uart_dev::UartWriter::new(uart_tx);
        low_prio_spawner
            .spawn(esb::uart_dev::task(uart_writer))
            .ok();
    }

    // Read from pipe and write to cache
    let server_reader = esb::server::Reader::new(pipe_reader);
    low_prio_spawner
        .spawn(esb::server::task_reader(server_reader))
        .ok();

    // Start web server
    let server = esb::server::Server::new(wifi_stack);
    low_prio_spawner
        .spawn(esb::server::task_server(server))
        .ok();
}
