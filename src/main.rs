use chrono::Utc;
use clap::Parser;
use image::{ImageBuffer, Luma, Rgba, RgbaImage};
use imageproc::contours;
use openssh::Session;
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// A utility to capture and process screenshots from reMarkable tablets
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// IP address of the reMarkable tablet
    #[clap(short = 'I', long = "ip-address", required = true)]
    ip_address: String,

    /// Directory to save the output files
    #[clap(short = 'd', long = "directory", default_value = ".")]
    output_dir: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    // Parse command-line arguments
    let args = Args::parse();
    let remarkable_ip = args.ip_address;
    let output_dir = args.output_dir;

    // Ensure output directory exists
    if !output_dir.exists() {
        fs::create_dir_all(&output_dir)?;
    }

    let session = Session::connect(
        format!("ssh://root@{}", remarkable_ip),
        openssh::KnownHosts::Add,
    )
    .await?;

    log::info!("✅ Connected to reMarkable at {}", remarkable_ip);

    // Find `xochitl` process ID
    let pid_output = session
        .command("/bin/pidof")
        .arg("xochitl")
        .output()
        .await?;
    let mut pid = String::from_utf8_lossy(&pid_output.stdout)
        .split_whitespace()
        .next()
        .ok_or("Could not find xochitl process ID")?
        .to_string();
    log::info!("🆔 Found xochitl PID: {}", pid);

    // Find framebuffer memory address
    // First check if this process has the right mapping
    let maps_check = session
        .command("grep")
        .args(["-C1", "/dev/fb0", &format!("/proc/{}/maps", pid)])
        .output()
        .await?;

    if maps_check.stdout.is_empty() {
        // If the first PID doesn't have the right mapping, find one that does
        let pids_output = String::from_utf8_lossy(&pid_output.stdout);
        let all_pids = pids_output.split_whitespace().collect::<Vec<&str>>();

        let mut found_pid = None;
        for test_pid in all_pids {
            let check = session
                .command("grep")
                .args(["-C1", "/dev/fb0", &format!("/proc/{}/maps", test_pid)])
                .output()
                .await?;

            if !check.stdout.is_empty() {
                found_pid = Some(test_pid.to_string());
                break;
            }
        }

        if let Some(p) = found_pid {
            log::info!("🔄 Switching to PID {} which has fb0 mapping", p);
            pid.clear();
            pid.push_str(&p);
        } else {
            return Err("Could not find any xochitl process with /dev/fb0 mapping".into());
        }
    }

    // Get the address after /dev/fb0 mapping
    let address_cmd = format!(
        "grep -C1 '/dev/fb0' /proc/{}/maps | tail -n1 | sed 's/-.*$//'",
        pid
    );

    let address_output = session
        .command("sh")
        .arg("-c")
        .arg(&address_cmd)
        .output()
        .await?;

    let skip_bytes_hex = String::from_utf8_lossy(&address_output.stdout)
        .trim()
        .to_string();
    let skip_bytes = u64::from_str_radix(&skip_bytes_hex, 16)? + 7;
    log::info!(
        "📍 Found framebuffer at address: 0x{} + 7 = {}",
        skip_bytes_hex,
        skip_bytes
    );

    // Calculate window size
    let width = 1872;
    let height = 1404;

    let (bytes_per_pixel, pixel_format, transpose) = (2, "gray16", "transpose=2,hflip"); // 90° clockwise and horizontal flip

    let window_bytes = width * height * bytes_per_pixel;
    log::info!(
        "📏 Window size: {}x{} ({}B per pixel, {} total)",
        width,
        height,
        bytes_per_pixel,
        window_bytes
    );

    // Create command to extract framebuffer data
    let dd_cmd = format!(
        "{{ dd bs=1 skip={} count=0 && dd bs={} count=1; }} < /proc/{}/mem 2>/dev/null",
        skip_bytes, window_bytes, pid
    );

    log::info!("📤 Extracting framebuffer data...");
    let fb_data = session
        .command("sh")
        .arg("-c")
        .arg(&dd_cmd)
        .output()
        .await?;

    // Save raw data to temp file in the output directory
    let temp_file = output_dir.join("remarkable_fb.raw");
    let mut file = File::create(&temp_file)?;
    file.write_all(&fb_data.stdout)?;
    log::info!("💾 Saved raw framebuffer to {}", temp_file.display());

    // Build ffmpeg filter chain
    let mut filters = String::from(transpose);
    filters.push_str(",curves=all=0.045/0 0.06/1");

    // Convert raw framebuffer to image using ffmpeg
    let now = Utc::now();
    let formatted_datetime = format!("{}-remarkable-screen.png", now.format("%m-%d-%Y-%H-%M-%S"));
    let output_file = output_dir.join(&formatted_datetime);
    let status = Command::new("ffmpeg")
        .args([
            "-f",
            "rawvideo",
            "-pixel_format",
            pixel_format,
            "-video_size",
            &format!("{}x{}", width, height),
            "-i",
            &temp_file.to_string_lossy(),
            "-vf",
            &filters,
            "-y",
            &output_file.to_string_lossy(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;

    if status.success() {
        log::info!(
            "🖼️ Converted framebuffer to image: {}",
            output_file.display()
        );
        // Clean up temporary file
        fs::remove_file(&temp_file)?;
    } else {
        return Err("Failed to convert framebuffer to image".into());
    }

    let img = image::open(&output_file)?;

    // Convert to grayscale if not already
    let gray_img = img.to_luma8();

    // Set threshold to isolate handwriting (assuming dark writing on light background)
    let threshold = 200; // Adjust as needed for your images

    // Define UI exclusion zone (menu button in top-left)
    let ui_exclude_x = 200; // Exclude x < this value
    let ui_exclude_y = 200; // Exclude y < this value
                            // Create a binary image to isolate the handwriting
    let binary_img = ImageBuffer::from_fn(gray_img.width(), gray_img.height(), |x, y| {
        if x < ui_exclude_x && y < ui_exclude_y {
            return Luma([255]); // Mark as background
        }
        let pixel = gray_img.get_pixel(x, y).0[0];
        if pixel < threshold {
            Luma([0]) // Black - this is handwriting
        } else {
            Luma([255]) // White - this is background
        }
    });

    // Find contours in the binary image
    let contours = contours::find_contours::<i32>(&binary_img);

    // Create visualization of contours for debugging
    let mut contour_vis = RgbaImage::new(gray_img.width(), gray_img.height());
    // Fill with white background
    for pixel in contour_vis.pixels_mut() {
        *pixel = Rgba([255, 255, 255, 255]);
    }

    // Calculate bounding box for all content of interest
    let mut min_x = gray_img.width();
    let mut min_y = gray_img.height();
    let mut max_x = 150;
    let mut max_y = 0;

    let mut found_contours = 0;
    let mut large_contours = 0;

    // Filter out small noise contours
    let min_contour_size = 100; // Adjust this threshold as needed

    for contour in contours {
        found_contours += 1;

        // Skip very small contours (likely noise)
        if contour.points.len() < min_contour_size {
            continue;
        }

        large_contours += 1;

        // Draw contour for visualization
        for point in &contour.points {
            if point.x >= 0
                && point.y >= 0
                && point.x < gray_img.width() as i32
                && point.y < gray_img.height() as i32
            {
                contour_vis.put_pixel(point.x as u32, point.y as u32, Rgba([255, 0, 0, 255]));
            }
        }

        // Update bounding box
        for point in &contour.points {
            if point.x >= 0
                && point.y >= 0
                && point.x < gray_img.width() as i32
                && point.y < gray_img.height() as i32
            {
                min_x = min_x.min(point.x as u32);
                min_y = min_y.min(point.y as u32);
                max_x = max_x.max(point.x as u32);
                max_y = max_y.max(point.y as u32);
            }
        }
    }

    log::info!(
        "Found {} contours, {} significant",
        found_contours,
        large_contours
    );

    // Add padding to the bounding box
    let padding = 50;
    let min_x = min_x.saturating_sub(padding);
    let min_y = min_y.saturating_sub(padding);
    let max_x = (max_x + padding).min(gray_img.width() - 1);
    let max_y = (max_y + padding).min(gray_img.height() - 1);

    // If we found a valid bounding box (content of interest)
    if min_x < max_x && min_y < max_y && large_contours > 0 {
        // Crop to the bounding box region
        let width = max_x - min_x + 1;
        let height = max_y - min_y + 1;

        log::info!(
            "📏 Content bounding box: ({}, {}) to ({}, {}), size: {}x{}",
            min_x,
            min_y,
            max_x,
            max_y,
            width,
            height
        );

        // Create cropped image
        let cropped = img.crop_imm(min_x, min_y, width, height);

        // Save cropped image
        let output_stem = Path::new(&formatted_datetime)
            .file_stem()
            .unwrap()
            .to_str()
            .unwrap();
        let cropped_path = output_dir.join(format!("{}_cropped.png", output_stem));
        cropped.save(&cropped_path)?;
        log::info!("✅ Saved cropped content to: {}", cropped_path.display());
        println!("{}", cropped_path.display());
    } else {
        log::info!("⚠️ No significant content found in the image");
    }

    Ok(())
}
