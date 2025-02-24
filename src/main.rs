use image::{ImageBuffer, Luma, Rgba, RgbaImage};
use imageproc::contours;
use openssh::Session;
use std::{env, fs, fs::File, io::Write, path::Path, process::Command};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let remarkable_ip = env::var("REMARKABLE_IP").expect("Could not access REMARKABLE_IP env var.");

    let session = Session::connect(
        format!("ssh://root@{}", remarkable_ip),
        openssh::KnownHosts::Add,
    )
    .await?;

    println!("✅ Connected to reMarkable at {}", remarkable_ip);

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
    println!("🆔 Found xochitl PID: {}", pid);

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
            println!("🔄 Switching to PID {} which has fb0 mapping", p);
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
    println!(
        "📍 Found framebuffer at address: 0x{} + 7 = {}",
        skip_bytes_hex, skip_bytes
    );

    // Calculate window size
    let width = 1872;
    let height = 1404;

    // Get byte format from env or use default
    // let byte_correction =
    //     env::var("BYTE_CORRECTION").unwrap_or_else(|_| "false".to_string()) == "true";
    // let color_correction =
    //     env::var("COLOR_CORRECTION").unwrap_or_else(|_| "false".to_string()) == "true";

    let (bytes_per_pixel, pixel_format, transpose) = (2, "gray16", "transpose=2,hflip"); // 90° clockwise and horizontal flip

    let window_bytes = width * height * bytes_per_pixel;
    println!(
        "📏 Window size: {}x{} ({}B per pixel, {} total)",
        width, height, bytes_per_pixel, window_bytes
    );

    // Create command to extract framebuffer data
    let dd_cmd = format!(
        "{{ dd bs=1 skip={} count=0 && dd bs={} count=1; }} < /proc/{}/mem 2>/dev/null",
        skip_bytes, window_bytes, pid
    );

    println!("📤 Extracting framebuffer data...");
    let fb_data = session
        .command("sh")
        .arg("-c")
        .arg(&dd_cmd)
        .output()
        .await?;

    // Save raw data to temp file
    let temp_file = "remarkable_fb.raw";
    let mut file = File::create(temp_file)?;
    file.write_all(&fb_data.stdout)?;
    println!("💾 Saved raw framebuffer to {}", temp_file);

    // Build ffmpeg filter chain
    let mut filters = String::from(transpose);
    filters.push_str(",curves=all=0.045/0 0.06/1");

    // Convert raw framebuffer to image using ffmpeg
    let output_file = "remarkable_screen.png";
    let status = Command::new("ffmpeg")
        .args([
            "-f",
            "rawvideo",
            "-pixel_format",
            pixel_format,
            "-video_size",
            &format!("{}x{}", width, height),
            "-i",
            temp_file,
            "-vf",
            &filters,
            "-y",
            output_file,
        ])
        .status()?;

    if status.success() {
        println!("🖼️ Converted framebuffer to image: {}", output_file);
        // Clean up temporary file
        fs::remove_file(temp_file)?;
    } else {
        return Err("Failed to convert framebuffer to image".into());
    }

    let img = image::open(output_file)?;

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

    println!(
        "Found {} contours, {} significant",
        found_contours, large_contours
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

        println!(
            "📏 Content bounding box: ({}, {}) to ({}, {}), size: {}x{}",
            min_x, min_y, max_x, max_y, width, height
        );

        // Create cropped image
        let cropped = img.crop_imm(min_x, min_y, width, height);

        // Save cropped image
        let output_path = format!(
            "{}_cropped.png",
            Path::new(&output_file)
                .file_stem()
                .unwrap()
                .to_str()
                .unwrap()
        );
        cropped.save(&output_path)?;
        println!("✅ Saved cropped content to: {}", output_path);
    } else {
        println!("⚠️ No significant content found in the image");
    }

    // // Alternative approach using the image crate - reading the exact number of bytes
    // let direct_output = "remarkable_screen_direct.png";
    // println!("📷 Creating direct image with image crate...");
    //
    // if byte_correction {
    //     // For 16-bit grayscale, make sure we have complete pixels
    //     let bytes_needed = width * height * 2;
    //     if fb_data.stdout.len() >= bytes_needed {
    //         let img_data = &fb_data.stdout[0..bytes_needed];
    //         let mut raw_data = Vec::new();
    //
    //         // Convert from u8 slice to u16 slice - handle endianness if needed
    //         for i in (0..img_data.len()).step_by(2) {
    //             if i + 1 < img_data.len() {
    //                 // Assuming little-endian format
    //                 let pixel = u16::from_le_bytes([img_data[i], img_data[i + 1]]);
    //                 raw_data.push(pixel);
    //             }
    //         }
    //
    //         let img: ImageBuffer<Luma<u16>, Vec<u16>> = ImageBuffer::from_raw(
    //             width.try_into().unwrap(),
    //             height.try_into().unwrap(),
    //             raw_data,
    //         )
    //         .ok_or("Failed to create 16-bit image from framebuffer data")?;
    //
    //         img.save(direct_output)?;
    //         println!("📸 Saved direct 16-bit image to {}", direct_output);
    //     } else {
    //         println!("⚠️ Not enough data for direct 16-bit image conversion");
    //     }
    // } else {
    //     // For 8-bit grayscale
    //     let bytes_needed = width * height;
    //     if fb_data.stdout.len() >= bytes_needed {
    //         let img_data = &fb_data.stdout[0..bytes_needed];
    //         let img: ImageBuffer<Luma<u8>, _> = ImageBuffer::from_raw(
    //             width.try_into().unwrap(),
    //             height.try_into().unwrap(),
    //             img_data.to_vec(),
    //         )
    //         .ok_or("Failed to create 8-bit image from framebuffer data")?;
    //
    //         img.save(direct_output)?;
    //         println!("📸 Saved direct 8-bit image to {}", direct_output);
    //     } else {
    //         println!("⚠️ Not enough data for direct image conversion");
    //     }
    // }

    Ok(())
}
