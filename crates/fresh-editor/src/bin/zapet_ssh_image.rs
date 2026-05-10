use fresh::services::clipboard::{
    copy_to_system_clipboard, get_system_clipboard_image_png, get_system_clipboard_text,
};
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const SSH_OPTS: &[&str] = &[
    "-o",
    "BatchMode=yes",
    "-o",
    "ConnectTimeout=5",
    "-o",
    "ConnectionAttempts=1",
    "-o",
    "ServerAliveInterval=3",
    "-o",
    "ServerAliveCountMax=1",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    Stdout,
    Clipboard,
}

#[derive(Debug)]
struct Args {
    remote: String,
    remote_dir: Option<String>,
    number: usize,
    mode: OutputMode,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("zapet-ssh-image: {}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let remote_dir = match args.remote_dir {
        Some(dir) => dir,
        None => {
            let home = remote_home(&args.remote)?;
            format!("{}/.tmp/.zapet/images", home.trim_end_matches('/'))
        }
    };

    if !remote_dir.starts_with('/') {
        return Err("remote image directory must be absolute".to_string());
    }

    let source = clipboard_image_source()?;
    let timestamp = chrono::Local::now().format("image-%Y-%m-%d-%H-%M-%S");
    let remote_path = allocate_remote_path(
        &args.remote,
        &remote_dir,
        &format!("{}", timestamp),
        source.extension,
    )?;

    create_remote_dir(&args.remote, &remote_dir)?;
    upload_file(&args.remote, &source.local_path, &remote_path)?;

    if source.remove_after_upload {
        let _ = std::fs::remove_file(&source.local_path);
    }

    let markdown = format!(
        "[Image #{}]({})",
        args.number,
        markdown_path_target(&remote_path)
    );
    match args.mode {
        OutputMode::Stdout => println!("{}", markdown),
        OutputMode::Clipboard => copy_to_system_clipboard(&markdown, false, true),
    }

    Ok(())
}

fn parse_args() -> Result<Args, String> {
    let mut remote = env::var("ZAPET_SSH_REMOTE").ok();
    let mut remote_dir = env::var("ZAPET_SSH_REMOTE_DIR").ok();
    let mut number = env::var("ZAPET_IMAGE_NUMBER")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1);
    let mut mode = OutputMode::Stdout;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--remote" => remote = args.next(),
            "--remote-dir" => remote_dir = args.next(),
            "--number" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--number requires a value".to_string())?;
                number = value
                    .parse::<usize>()
                    .map_err(|_| "--number must be a positive integer".to_string())?;
                if number == 0 {
                    return Err("--number must be a positive integer".to_string());
                }
            }
            "--stdout" => mode = OutputMode::Stdout,
            "--clipboard" => mode = OutputMode::Clipboard,
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {}", other)),
        }
    }

    let remote = remote.ok_or_else(|| {
        "missing SSH target; pass --remote user@host or set ZAPET_SSH_REMOTE".to_string()
    })?;

    Ok(Args {
        remote,
        remote_dir,
        number,
        mode,
    })
}

fn print_usage() {
    println!(
        "Usage: zapet-ssh-image --remote user@host [--remote-dir /abs/path] [--number N] [--stdout|--clipboard]"
    );
}

struct ClipboardImageSource {
    local_path: PathBuf,
    extension: &'static str,
    remove_after_upload: bool,
}

fn clipboard_image_source() -> Result<ClipboardImageSource, String> {
    if let Some(text) = get_system_clipboard_text() {
        if let Some(path) = clipboard_text_image_path(&text) {
            if path.is_file() {
                return Ok(ClipboardImageSource {
                    extension: supported_image_extension(&path).unwrap_or("png"),
                    local_path: path,
                    remove_after_upload: false,
                });
            }
        }
    }

    let bytes = get_system_clipboard_image_png()
        .ok_or_else(|| "no image found in clipboard".to_string())?;
    let path = env::temp_dir().join(format!(
        "zapet-image-{}-{}.png",
        std::process::id(),
        chrono::Local::now().format("%Y-%m-%d-%H-%M-%S")
    ));
    std::fs::write(&path, bytes).map_err(|e| format!("failed to write temp image: {}", e))?;

    Ok(ClipboardImageSource {
        local_path: path,
        extension: "png",
        remove_after_upload: true,
    })
}

fn supported_image_extension(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("png"),
        "jpg" | "jpeg" => Some("jpg"),
        "gif" => Some("gif"),
        "webp" => Some("webp"),
        "bmp" => Some("bmp"),
        "tif" | "tiff" => Some("tiff"),
        _ => None,
    }
}

fn clipboard_text_image_path(text: &str) -> Option<PathBuf> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.lines().count() > 1 {
        return None;
    }

    let trimmed = trimmed
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| {
            trimmed
                .strip_prefix('\'')
                .and_then(|s| s.strip_suffix('\''))
        })
        .unwrap_or(trimmed);

    let path_text = if let Some(rest) = trimmed.strip_prefix("file://localhost") {
        percent_decode(rest)?
    } else if let Some(rest) = trimmed.strip_prefix("file://") {
        percent_decode(rest)?
    } else {
        trimmed.to_string()
    };

    let path = if let Some(rest) = path_text.strip_prefix("~/") {
        dirs::home_dir()?.join(rest)
    } else {
        PathBuf::from(path_text)
    };

    if !path.is_absolute() || supported_image_extension(&path).is_none() {
        return None;
    }

    Some(path)
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            let value = u8::from_str_radix(hex, 16).ok()?;
            decoded.push(value);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn remote_home(remote: &str) -> Result<String, String> {
    let output = Command::new("ssh")
        .args(SSH_OPTS)
        .arg(remote)
        .arg("printf %s \"$HOME\"")
        .output()
        .map_err(|e| format!("failed to run ssh: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "failed to read remote home: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let home = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if home.is_empty() {
        return Err("remote home is empty".to_string());
    }
    Ok(home)
}

fn allocate_remote_path(
    remote: &str,
    remote_dir: &str,
    timestamp: &str,
    extension: &str,
) -> Result<String, String> {
    for suffix in 0..1000 {
        let filename = if suffix == 0 {
            format!("{}.{}", timestamp, extension)
        } else {
            format!("{}-{}.{}", timestamp, suffix + 1, extension)
        };
        let remote_path = format!("{}/{}", remote_dir.trim_end_matches('/'), filename);
        if !remote_path_exists(remote, &remote_path)? {
            return Ok(remote_path);
        }
    }

    Err("could not allocate remote image filename".to_string())
}

fn remote_path_exists(remote: &str, path: &str) -> Result<bool, String> {
    let status = Command::new("ssh")
        .args(SSH_OPTS)
        .arg(remote)
        .arg(format!("test -e -- {}", shell_quote(path)))
        .status()
        .map_err(|e| format!("failed to run ssh: {}", e))?;
    Ok(status.success())
}

fn create_remote_dir(remote: &str, remote_dir: &str) -> Result<(), String> {
    let status = Command::new("ssh")
        .args(SSH_OPTS)
        .arg(remote)
        .arg(format!("mkdir -p -- {}", shell_quote(remote_dir)))
        .status()
        .map_err(|e| format!("failed to run ssh: {}", e))?;
    if status.success() {
        Ok(())
    } else {
        Err("failed to create remote image directory".to_string())
    }
}

fn upload_file(remote: &str, local_path: &Path, remote_path: &str) -> Result<(), String> {
    let status = Command::new("scp")
        .arg("-q")
        .args(SSH_OPTS)
        .arg(local_path)
        .arg(format!("{}:{}", remote, shell_quote(remote_path)))
        .status()
        .map_err(|e| format!("failed to run scp: {}", e))?;
    if status.success() {
        Ok(())
    } else {
        Err("failed to upload image with scp".to_string())
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn markdown_path_target(path: &str) -> String {
    if path
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '(' | ')' | '<' | '>'))
    {
        format!("<{}>", path)
    } else {
        path.to_string()
    }
}
