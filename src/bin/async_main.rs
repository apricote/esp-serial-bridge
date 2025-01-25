#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esb;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::uart::Uart;
use esp_println::println;
use log::info;

extern crate alloc;

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(72 * 1024);

    esp_println::logger::init_logger_from_env();

    let timer0 = esp_hal::timer::systimer::SystemTimer::new(peripherals.SYSTIMER);
    esp_hal_embassy::init(timer0.alarm0);

    let rng = esp_hal::rng::Rng::new(peripherals.RNG);

    info!("Embassy initialized!");

    let (wifi_controller, wifi_stack, wifi_runner) = esb::wifi::init(
        peripherals.TIMG0,
        peripherals.RADIO_CLK,
        peripherals.WIFI,
        rng.clone(),
    );

    spawner.spawn(esb::wifi::connection(wifi_controller)).ok();
    spawner.spawn(esb::wifi::net_task(wifi_runner)).ok();

    let ring_buffer = esb::ringbuffer::RingBuffer::<{ esb::RINGBUFFER_SIZE }>::new();

    let (uart_rx, uart_tx) = Uart::new(peripherals.UART1, Default::default())
        .unwrap()
        .with_rx(peripherals.GPIO2)
        .with_tx(peripherals.GPIO3)
        .into_async()
        .split();
    let uart_reader = esb::uart::UartReader::new(uart_rx, ring_buffer);
    let uart_writer = esb::uart_dev::UartWriter::new(uart_tx);

    spawner.spawn(esb::uart::task(uart_reader)).ok();
    spawner.spawn(esb::uart_dev::task(uart_writer)).ok();

    loop {
        if wifi_stack.is_link_up() {
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    println!("Waiting to get IP address...");
    loop {
        if let Some(config) = wifi_stack.config_v4() {
            println!("Got IP: {}", config.address);
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }
}
