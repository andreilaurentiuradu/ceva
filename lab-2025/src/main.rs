//! Smart Basketball Hoop - Simplified Logic
//! 
//! Simplified logic for shot detection:
//! 1. HC-SR04 detects ball (trig=25, echo=24)
//! 2. After 3 seconds without ring detection -> LEDs red
//! 3. Detection in 2+ seconds -> LEDs blue + point
//! 4. Detection under 2 seconds -> LEDs green + point

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer, Instant};
use embassy_sync::{blocking_mutex::raw::ThreadModeRawMutex, mutex::Mutex};
use embassy_rp::{
    gpio::{Level, Output, Input, Pull},
    uart::{self, Uart, Config as UartConfig, Blocking},
    peripherals::*,
    bind_interrupts,
};
use {defmt_rtt as _, panic_probe as _};
use defmt::*;

// Game state
static SCORE: Mutex<ThreadModeRawMutex, u32> = Mutex::new(0);

bind_interrupts!(
    struct Irqs {
        UART0_IRQ => uart::InterruptHandler<UART0>;
        UART1_IRQ => uart::InterruptHandler<UART1>;
    }
);

#[derive(Clone, Copy, Debug)]
enum ShotResult {
    Miss,      // Red
    Good,      // Blue  
    Perfect,   // Green
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    info!("Smart Hoop Simple Logic Starting...");
    
    let peripherals = embassy_rp::init(Default::default());

    // Main HC-SR04 for ball detection (trig=25, echo=24)
    let hc_sr04 = HcSr04Sensor::new(
        Output::new(peripherals.PIN_25, Level::Low), // Trig
        Input::new(peripherals.PIN_24, Pull::None),  // Echo
    );

    // 3 IOE-SR05 ring sensors (trig: 4,9,14 / echo: 5,10,15)
    let ring_sensor1 = IoeSr05Sensor::new(
        Output::new(peripherals.PIN_4, Level::Low),  // Trig
        Input::new(peripherals.PIN_5, Pull::None),   // Echo
        1
    );
    
    let ring_sensor2 = IoeSr05Sensor::new(
        Output::new(peripherals.PIN_9, Level::Low),  // Trig
        Input::new(peripherals.PIN_10, Pull::None),  // Echo
        2
    );
    
    let ring_sensor3 = IoeSr05Sensor::new(
        Output::new(peripherals.PIN_14, Level::Low), // Trig
        Input::new(peripherals.PIN_15, Pull::None),  // Echo
        3
    );

    // TM1637 4-digit 7-segment Display (clk=21, dio=22)
    let display = TM1637Display::new(
        Output::new(peripherals.PIN_21, Level::Low), // CLK
        Output::new(peripherals.PIN_22, Level::Low), // DIO
    );

    // RGB LEDs (pins 17, 19, 20)
    let leds = RgbLeds::new(
        Output::new(peripherals.PIN_17, Level::Low), // Red
        Output::new(peripherals.PIN_19, Level::Low), // Green
        Output::new(peripherals.PIN_20, Level::Low), // Blue
    );

    info!("Hardware initialized");

    // Spawn tasks
    spawner.spawn(game_logic_task(hc_sr04, ring_sensor1, ring_sensor2, ring_sensor3)).unwrap();
    spawner.spawn(display_task(display)).unwrap();
    spawner.spawn(led_task(leds)).unwrap();

    info!("Ready to play!");

    // Main loop
    loop {
        Timer::after(Duration::from_secs(5)).await;
        let score = SCORE.lock().await;
        info!("Current Score: {}", *score);
    }
}

#[embassy_executor::task]
async fn game_logic_task(
    mut hc_sr04: HcSr04Sensor,
    mut ring1: IoeSr05Sensor,
    mut ring2: IoeSr05Sensor, 
    mut ring3: IoeSr05Sensor
) {
    info!("Game logic started");
    
    let mut current_result = ShotResult::Miss;
    
    loop {
        // Wait for HC-SR04 to detect ball
        if let Ok(distance) = hc_sr04.read_distance().await {
            if distance < 50 { // Ball detected in front of hoop
                info!("Ball detected at {}cm! Starting timer...", distance);
                
                let shot_start = Instant::now();
                let mut ball_detected_in_ring = false;
                
                // Monitor ring sensors for max 3 seconds
                while shot_start.elapsed() < Duration::from_secs(3) && !ball_detected_in_ring {
                    // Check the 3 ring sensors
                    let ring1_dist = ring1.read_distance().await.unwrap_or(400);
                    let ring2_dist = ring2.read_distance().await.unwrap_or(400);
                    let ring3_dist = ring3.read_distance().await.unwrap_or(400);
                    
                    // Detect if ball passed through ring (short distance)
                    if ring1_dist < 60 || ring2_dist < 60 || ring3_dist < 60 {
                        ball_detected_in_ring = true;
                        let elapsed = shot_start.elapsed();
                        
                        if elapsed < Duration::from_secs(2) {
                            // Under 2 seconds -> Green LEDs (Perfect)
                            current_result = ShotResult::Perfect;
                            info!("PERFECT SHOT! Time: {}ms", elapsed.as_millis());
                        } else {
                            // 2+ seconds -> Blue LEDs (Good) 
                            current_result = ShotResult::Good;
                            info!("GOOD SHOT! Time: {}ms", elapsed.as_millis());
                        }
                        
                        // Increment score
                        {
                            let mut score = SCORE.lock().await;
                            *score += 1;
                            info!("SCORE! New total: {}", *score);
                        }
                        
                        break;
                    }
                    
                    Timer::after(Duration::from_millis(50)).await;
                }
                
                // If nothing detected in 3 seconds -> Miss (red)
                if !ball_detected_in_ring {
                    current_result = ShotResult::Miss;
                    info!("MISS! No detection in ring sensors");
                }
                
                // Display result for 2 seconds
                set_global_result(current_result).await;
                Timer::after(Duration::from_secs(2)).await;
                
                // Reset to idle
                set_global_result(ShotResult::Miss).await;
                Timer::after(Duration::from_secs(1)).await; // Pause between shots
            }
        }
        
        Timer::after(Duration::from_millis(100)).await;
    }
}

// Global result for inter-task communication
static CURRENT_RESULT: Mutex<ThreadModeRawMutex, ShotResult> = Mutex::new(ShotResult::Miss);

async fn set_global_result(result: ShotResult) {
    let mut current = CURRENT_RESULT.lock().await;
    *current = result;
}

#[embassy_executor::task]
async fn display_task(mut display: TM1637Display) {
    info!("Display task started");
    
    // Startup sequence
    display.show_text("HOOP").await;
    Timer::after(Duration::from_secs(1)).await;
    display.show_number(0).await;
    
    loop {
        let score = SCORE.lock().await;
        display.show_number(*score).await;
        drop(score);
        
        Timer::after(Duration::from_millis(200)).await;
    }
}

#[embassy_executor::task]
async fn led_task(mut leds: RgbLeds) {
    info!("LED task started");
    
    // Startup animation - purple
    leds.set_color([128, 0, 128]).await; // Purple
    Timer::after(Duration::from_secs(1)).await;
    leds.clear().await;
    
    loop {
        let result = CURRENT_RESULT.lock().await;
        
        match *result {
            ShotResult::Miss => {
                // Red LEDs for miss or idle
                leds.set_color([255, 0, 0]).await;
            }
            ShotResult::Good => {
                // Blue LEDs for good shot (2+ seconds)
                leds.set_color([0, 0, 255]).await;
            }
            ShotResult::Perfect => {
                // Green LEDs for perfect shot (under 2 seconds)
                leds.set_color([0, 255, 0]).await;
            }
        }
        
        Timer::after(Duration::from_millis(100)).await;
    }
}

// Sensor and display implementations

struct HcSr04Sensor {
    trigger: Output<'static>,
    echo: Input<'static>,
}

impl HcSr04Sensor {
    fn new(trigger: Output<'static>, echo: Input<'static>) -> Self {
        Self { trigger, echo }
    }

    async fn read_distance(&mut self) -> Result<u16, ()> {
        // HC-SR04 protocol
        self.trigger.set_low();
        Timer::after(Duration::from_micros(2)).await;
        
        self.trigger.set_high();
        Timer::after(Duration::from_micros(10)).await;
        self.trigger.set_low();
        
        // Wait for echo start
        let start_time = Instant::now();
        let timeout = Duration::from_millis(30);
        
        while self.echo.is_low() {
            if start_time.elapsed() > timeout {
                return Err(());
            }
            Timer::after(Duration::from_micros(1)).await;
        }
        
        let echo_start = Instant::now();
        
        // Wait for echo end
        while self.echo.is_high() {
            if echo_start.elapsed() > timeout {
                return Err(());
            }
            Timer::after(Duration::from_micros(1)).await;
        }
        
        let echo_duration = echo_start.elapsed().as_micros();
        let distance_cm = (echo_duration as f32 * 0.01715) as u16;
        
        Ok(distance_cm.min(400))
    }
}

struct IoeSr05Sensor {
    trigger: Output<'static>,
    echo: Input<'static>,
    sensor_id: u8,
}

impl IoeSr05Sensor {
    fn new(trigger: Output<'static>, echo: Input<'static>, id: u8) -> Self {
        Self { trigger, echo, sensor_id: id }
    }

    async fn read_distance(&mut self) -> Result<u16, ()> {
        // IOE-SR05 can work in GPIO mode like HC-SR04
        self.trigger.set_low();
        Timer::after(Duration::from_micros(2)).await;
        
        self.trigger.set_high();
        Timer::after(Duration::from_micros(10)).await;
        self.trigger.set_low();
        
        // Wait for echo start
        let start_time = Instant::now();
        let timeout = Duration::from_millis(30);
        
        while self.echo.is_low() {
            if start_time.elapsed() > timeout {
                warn!("Sensor {} timeout waiting for echo start", self.sensor_id);
                return Err(());
            }
            Timer::after(Duration::from_micros(1)).await;
        }
        
        let echo_start = Instant::now();
        
        // Wait for echo end
        while self.echo.is_high() {
            if echo_start.elapsed() > timeout {
                warn!("Sensor {} timeout waiting for echo end", self.sensor_id);
                return Err(());
            }
            Timer::after(Duration::from_micros(1)).await;
        }
        
        let echo_duration = echo_start.elapsed().as_micros();
        let distance_cm = (echo_duration as f32 * 0.01715) as u16;
        
        Ok(distance_cm.min(400))
    }
}

struct TM1637Display {
    clk: Output<'static>,
    dio: Output<'static>,
}

impl TM1637Display {
    fn new(clk: Output<'static>, dio: Output<'static>) -> Self {
        Self { clk, dio }
    }

    async fn show_number(&mut self, number: u32) {
        // Simplified TM1637 implementation
        info!("Display: {}", number);
        
        // In real implementation, would send actual TM1637 commands
        // For now, just pulse the pins to show activity
        for _ in 0..8 {
            self.clk.set_high();
            Timer::after(Duration::from_micros(1)).await;
            self.clk.set_low();
            Timer::after(Duration::from_micros(1)).await;
        }
    }

    async fn show_text(&mut self, text: &str) {
        info!("Display: {}", text);
        
        // Pulse pins for activity indication
        for _ in 0..16 {
            self.dio.set_high();
            Timer::after(Duration::from_micros(1)).await;
            self.dio.set_low();
            Timer::after(Duration::from_micros(1)).await;
        }
    }
}

struct RgbLeds {
    red_pin: Output<'static>,
    green_pin: Output<'static>,
    blue_pin: Output<'static>,
}

impl RgbLeds {
    fn new(red_pin: Output<'static>, green_pin: Output<'static>, blue_pin: Output<'static>) -> Self {
        Self { red_pin, green_pin, blue_pin }
    }

    async fn set_color(&mut self, rgb: [u8; 3]) {
        info!("LEDs: RGB({}, {}, {})", rgb[0], rgb[1], rgb[2]);
        
        // Set RGB pins based on color values
        // For simplicity, using digital on/off (not PWM)
        if rgb[0] > 128 {
            self.red_pin.set_high();
        } else {
            self.red_pin.set_low();
        }
        
        if rgb[1] > 128 {
            self.green_pin.set_high();
        } else {
            self.green_pin.set_low();
        }
        
        if rgb[2] > 128 {
            self.blue_pin.set_high();
        } else {
            self.blue_pin.set_low();
        }
    }

    async fn clear(&mut self) {
        self.set_color([0, 0, 0]).await;
    }
}