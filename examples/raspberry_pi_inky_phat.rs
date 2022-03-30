extern crate linux_embedded_hal;

use linux_embedded_hal::gpio_cdev;
use linux_embedded_hal::spidev::{SpiModeFlags, SpidevOptions};
use linux_embedded_hal::{CdevPin, Delay, Spidev};

extern crate ssd1675;

use ssd1675::{Builder, Color, Dimensions, Display, GraphicDisplay, Rotation};

// Graphics
#[macro_use]
extern crate embedded_graphics;

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::prelude::*;
use embedded_graphics::text::Text;

// Font
extern crate profont;

use profont::{PROFONT_12_POINT, PROFONT_14_POINT, PROFONT_24_POINT, PROFONT_9_POINT};

use std::process::Command;
use std::thread::sleep;
use std::time::Duration;
use std::{fs, io};

// Activate SPI, GPIO in raspi-config needs to be run with sudo because of some sysfs_gpio
// permission problems and follow-up timing problems
// see https://github.com/rust-embedded/rust-sysfs-gpio/issues/5 and follow-up issues

const ROWS: u16 = 212;
const COLS: u8 = 104;

#[rustfmt::skip]
const LUT: [u8; 70] = [
    // Phase 0     Phase 1     Phase 2     Phase 3     Phase 4     Phase 5     Phase 6
    // A B C D     A B C D     A B C D     A B C D     A B C D     A B C D     A B C D
    0b01001000, 0b10100000, 0b00010000, 0b00010000, 0b00010011, 0b00000000, 0b00000000,  // LUT0 - Black
    0b01001000, 0b10100000, 0b10000000, 0b00000000, 0b00000011, 0b00000000, 0b00000000,  // LUTT1 - White
    0b00000000, 0b00000000, 0b00000000, 0b00000000, 0b00000000, 0b00000000, 0b00000000,  // IGNORE
    0b01001000, 0b10100101, 0b00000000, 0b10111011, 0b00000000, 0b00000000, 0b00000000,  // LUT3 - Red
    0b00000000, 0b00000000, 0b00000000, 0b00000000, 0b00000000, 0b00000000, 0b00000000,  // LUT4 - VCOM

    // Duration            |  Repeat
    // A   B     C     D   |
    64,   12,   32,   12,    6,   // 0 Flash
    16,   8,    4,    4,     6,   // 1 clear
    4,    8,    8,    16,    16,  // 2 bring in the black
    2,    2,    2,    64,    32,  // 3 time for red
    2,    2,    2,    2,     2,   // 4 final black sharpen phase
    0,    0,    0,    0,     0,   // 5
    0,    0,    0,    0,     0    // 6
];

fn main() -> Result<(), std::io::Error> {
    // Configure SPI
    let mut spi = Spidev::open("/dev/spidev0.0").expect("SPI device");
    let options = SpidevOptions::new()
        .bits_per_word(8)
        .max_speed_hz(4_000_000)
        .mode(SpiModeFlags::SPI_MODE_0)
        .build();
    spi.configure(&options).expect("SPI configuration");

    // https://pinout.xyz/pinout/inky_phat
    // Configure Digital I/O Pins
    let mut gpio_chip = gpio_cdev::Chip::new("/dev/gpiochip0").expect("GPIO chip");

    // CSn is not actually used
    let line = gpio_chip.get_line(6).expect("CS line");
    let line_handle = line
        .request(gpio_cdev::LineRequestFlags::OUTPUT, 1, "spi_csn")
        .expect("CS line request");
    let cs = CdevPin::new(line_handle).expect("CS pin");

    let line = gpio_chip.get_line(17).expect("busy line");
    let line_handle = line
        .request(gpio_cdev::LineRequestFlags::INPUT, 1, "busy")
        .expect("busy line request");
    let busy = CdevPin::new(line_handle).expect("busy pin");

    let line = gpio_chip.get_line(22).expect("dc line");
    let line_handle = line
        .request(gpio_cdev::LineRequestFlags::OUTPUT, 1, "data_command")
        .expect("dc line request");
    let dc = CdevPin::new(line_handle).expect("dc pin");

    let line = gpio_chip.get_line(27).expect("reset line");
    let line_handle = line
        .request(gpio_cdev::LineRequestFlags::OUTPUT, 1, "reset")
        .expect("reset line request");
    let reset = CdevPin::new(line_handle).expect("reset pin");

    println!("Pins configured");

    // Initialise display controller
    let mut delay = Delay {};

    let controller = ssd1675::Interface::new(spi, cs, busy, dc, reset);

    let mut black_buffer = [0u8; ROWS as usize * COLS as usize / 8];
    let mut red_buffer = [0u8; ROWS as usize * COLS as usize / 8];
    let config = Builder::new()
        .dimensions(Dimensions {
            rows: ROWS,
            cols: COLS,
        })
        .rotation(Rotation::Rotate270)
        .lut(&LUT)
        .build()
        .expect("invalid configuration");
    let display = Display::new(controller, config);
    let mut display = GraphicDisplay::new(display, &mut black_buffer, &mut red_buffer);

    // Main loop. Displays CPU temperature, uname, and uptime every minute with a red Raspberry Pi
    // header.
    loop {
        display.reset(&mut delay).expect("error resetting display");
        println!("Reset and initialised");
        let one_minute = Duration::from_secs(60);

        display.clear(Color::White);
        println!("Clear");

        Text::new(
            "Raspberry Pi",
            Point::new(1, -4),
            MonoTextStyle::new(&PROFONT_24_POINT, Color::Red),
        )
        .draw(&mut display)
        .expect("error drawing text");

        if let Ok(cpu_temp) = read_cpu_temp() {
            Text::new(
                "CPU Temp:",
                Point::new(1, 30),
                MonoTextStyle::new(&PROFONT_14_POINT, Color::Black),
            )
            .draw(&mut display)
            .expect("error drawing text");
            Text::new(
                &format!("{:.1}°C", cpu_temp),
                Point::new(95, 34),
                MonoTextStyle::new(&PROFONT_12_POINT, Color::Black),
            )
            .draw(&mut display)
            .expect("error drawing text");
        }

        if let Some(uptime) = read_uptime() {
            Text::new(
                uptime.trim(),
                Point::new(1, 93),
                MonoTextStyle::new(&PROFONT_9_POINT, Color::Black),
            )
            .draw(&mut display)
            .expect("error drawing text");
        }

        if let Some(uname) = read_uname() {
            Text::new(
                uname.trim(),
                Point::new(1, 84),
                MonoTextStyle::new(&PROFONT_9_POINT, Color::Black),
            )
            .draw(&mut display)
            .expect("error drawing text");
        }

        display.update(&mut delay).expect("error updating display");
        println!("Update...");

        println!("Finished - going to sleep");
        display.deep_sleep()?;

        sleep(one_minute);
    }
}

fn read_cpu_temp() -> Result<f64, io::Error> {
    fs::read_to_string("/sys/class/thermal/thermal_zone0/temp")?
        .trim()
        .parse::<i32>()
        .map(|temp| temp as f64 / 1000.)
        .map_err(|err| io::Error::new(io::ErrorKind::Other, err))
}

fn read_uptime() -> Option<String> {
    Command::new("uptime")
        .arg("-p")
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        })
}

fn read_uname() -> Option<String> {
    Command::new("uname")
        .arg("-smr")
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        })
}
