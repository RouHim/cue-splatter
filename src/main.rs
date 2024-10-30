use argh::FromArgs;
use chardet::charset2encoding;
use encoding::DecoderTrap;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use levenshtein::levenshtein;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[derive(Debug, FromArgs)]
/// Split audio files based on cue sheets
struct CliArgs {
    /// only print the ffmpeg commands
    #[argh(switch)]
    dry_run: bool,

    /// tolerate audio file named inaccuracy
    #[argh(switch)]
    tolerate_audio_file_inaccuracy: bool,

    /// delete the original audio file
    #[argh(switch)]
    delete_original: bool,

    /// file or folder paths to parse
    /// default is "."
    #[argh(positional, greedy)]
    cue_file_or_folders: Vec<String>,
}

#[derive(Debug, Clone)]
struct CueSheet {
    cue_file_path: PathBuf,
    audio_file_path: PathBuf,
    file_name: String,
    tracks: Vec<Track>,
}

#[derive(Debug, Clone)]
struct Track {
    number: u32,
    title: Option<String>,
    artist: Option<String>,
    start_time: Option<CueDuration>,
    output_file: Option<PathBuf>,
    ffmpeg_command: Option<String>,
}

#[derive(Debug, Default, Copy, Clone)]
struct CueDuration {
    minutes: u32,
    seconds: u32,
    frames: u32,
}

fn main() {
    let mut cli_args: CliArgs = argh::from_env();
    if cli_args.cue_file_or_folders.is_empty() {
        cli_args.cue_file_or_folders.push(".".to_string());
    }

    check_tools(vec!["ffmpeg"]);

    let cue_file_paths = cli_args
        .cue_file_or_folders
        .iter()
        .flat_map(|input_path| find_cue_files(Path::new(input_path)))
        .collect();

    let_user_verify_cue_files(&cue_file_paths);

    let cue_sheets: Vec<CueSheet> = cue_file_paths
        .iter()
        .flat_map(parse_cue_file)
        .map(|cue_sheet| verify_cue_files(cue_sheet, cli_args.tolerate_audio_file_inaccuracy))
        .collect();

    let tracks: Vec<Track> = cue_sheets.iter().flat_map(build_ffmpeg_commands).collect();

    splitting_tracks(tracks, cli_args.dry_run);

    if !cli_args.dry_run && cli_args.delete_original {
        println!("🗑 Deleting original audio files ...");
        for cue_file in cue_sheets {
            std::fs::remove_file(cue_file.audio_file_path).unwrap();
        }
        println!("🎉 All original audio files have been deleted");
    }
}

fn let_user_verify_cue_files(cue_files: &Vec<PathBuf>) {
    println!("Found {} cue file(s):", cue_files.len());
    for cue_file in cue_files {
        println!("\t{}", cue_file.display());
    }
    println!();
    println!("Proceed with splitting? (Y/n)");

    // proceed if user enters y|Y or just hits ENTER
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();
    if !input.trim().is_empty() && !input.trim().eq_ignore_ascii_case("y") {
        println!("🚪 Exiting ...");
        std::process::exit(0);
    }
}

fn splitting_tracks(tracks: Vec<Track>, dry_run: bool) {
    if dry_run {
        println!("🚀 Dry run, only printing ffmpeg commands");
        for track in tracks {
            println!("{}", track.ffmpeg_command.unwrap_or("".to_string()));
        }
    } else {
        println!();

        run_ffmpeg_split_commands(tracks);

        println!("🎉 All tracks have been splitted");
    }
}

fn run_ffmpeg_split_commands(tracks: Vec<Track>) {
    let multi_progress_bar = MultiProgress::new();
    let mp_progress_bar = multi_progress_bar.add(
        ProgressBar::new(tracks.len() as u64)
            .with_style(
                ProgressStyle::default_bar()
                    .template("{msg}: {pos}/{len}")
                    .unwrap(),
            )
            .with_message("🚀 Splitting tracks ..."),
    );
    mp_progress_bar.enable_steady_tick(Duration::from_millis(100));

    tracks.par_iter().for_each(|track| {
        let split_command_bar = create_spinner(&multi_progress_bar, track);

        //  Run the actual ffmpeg command
        run_ffmpeg_split_command(track);

        split_command_bar.finish_and_clear();
        mp_progress_bar.inc(1);
    });

    mp_progress_bar.finish_and_clear();
}

fn create_spinner(multi_progress_bar: &MultiProgress, track: &Track) -> ProgressBar {
    let x = track.output_file.clone();
    let binding = x.unwrap();
    let output_file_name = binding.file_name().unwrap().to_str().unwrap();

    let split_command_bar = multi_progress_bar.add(
        ProgressBar::new_spinner()
            .with_style(
                ProgressStyle::default_spinner()
                    .template("{spinner} {wide_msg}")
                    .unwrap(),
            )
            .with_message(format!("Splitting into: {}", &output_file_name)),
    );

    split_command_bar.enable_steady_tick(Duration::from_millis(100));

    split_command_bar
}

fn run_ffmpeg_split_command(track: &Track) {
    let ffmpeg_command = track.ffmpeg_command.as_ref().unwrap();

    // Make sure all sub dirs exist
    let output_file = track.output_file.as_ref().unwrap();
    let output_dir = output_file.parent().unwrap();
    std::fs::create_dir_all(output_dir).unwrap();

    let output = Command::new("sh")
        .arg("-c")
        .arg(ffmpeg_command)
        .output()
        .expect("Failed to execute command");

    if !output.status.success() {
        println!("❌ FFMPEG command failed: {}", ffmpeg_command);
        println!("{}", String::from_utf8_lossy(&output.stdout));
        println!("{}", String::from_utf8_lossy(&output.stderr));
    }
}

fn verify_cue_files(cue_sheet: CueSheet, tolerate_audio_file_inaccuracy: bool) -> CueSheet {
    let mut cue_sheet = cue_sheet.clone();

    println!("🔍 Verifying cue file \"{}\"", cue_sheet.file_name);

    // Verify that the cue file exists
    if !cue_sheet.cue_file_path.exists() {
        eprintln!(
            "❌ The specified cue file was not found: {:?}",
            cue_sheet.cue_file_path
        );
        std::process::exit(1);
    }

    // Verify that the audio file name exists
    if !cue_sheet.audio_file_path.exists() {
        println!(
            "❌ The specified audio file was not found: {:?}",
            cue_sheet.audio_file_path
        );
        if tolerate_audio_file_inaccuracy {
            fix_audio_file_path_case(&mut cue_sheet);
        } else {
            std::process::exit(1);
        }
    };

    // Verify that there are tracks in the cue file
    if cue_sheet.tracks.is_empty() {
        eprintln!("❌ No tracks found in cue file {}", cue_sheet.file_name);
        std::process::exit(1);
    }

    // Verify that ffmpeg can process the input file
    // Example: ffmpeg -v error -i test.mp3 -f null -
    let output = Command::new("ffmpeg")
        .arg("-v")
        .arg("error")
        .arg("-i")
        .arg(&cue_sheet.audio_file_path)
        .arg("-f")
        .arg("null")
        .arg("-")
        .output()
        .expect("Failed to execute ffmpeg command");
    if !output.status.success() {
        eprintln!("❌ ffmpeg failed to process file {}", cue_sheet.file_name);
        eprintln!("{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        std::process::exit(1);
    }

    for track in &cue_sheet.tracks {
        // Ensure that the cue duration is present
        if track.start_time.is_none() {
            eprintln!("❌ No start time found for track {}", track.number);
            std::process::exit(1);
        }
    }

    println!("✅ Cue file is valid");

    cue_sheet
}

/// Fixes the case of the audio file path in the cue sheet
/// This happens when the case of the audio file path in the cue sheet does not match the actual file path
/// This is a common issue on Windows file systems
fn fix_audio_file_path_case(cue_sheet: &mut CueSheet) {
    let broken_file_name = cue_sheet.audio_file_path.file_name().unwrap();
    let parent_dir = cue_sheet.audio_file_path.parent().unwrap();
    let broken_file_name = broken_file_name.to_str().unwrap();
    let extension = cue_sheet
        .audio_file_path
        .extension()
        .unwrap()
        .to_str()
        .unwrap();

    let audio_files_in_same_dir: Vec<(PathBuf, usize)> = parent_dir
        .read_dir()
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().unwrap().is_file())
        // Find all audio files in the same directory
        .filter(|entry| {
            entry
                .path()
                .extension()
                .unwrap_or("".as_ref())
                .eq_ignore_ascii_case(extension)
        })
        .map(|entry| entry.path())
        // Calculate the levenshtein distance between the broken file name and the actual file name
        .map(|audio_entry| {
            let entry_file_name = audio_entry.file_name();
            (
                audio_entry.clone(),
                levenshtein(broken_file_name, entry_file_name.unwrap().to_str().unwrap()),
            )
        })
        .collect();

    if audio_files_in_same_dir.is_empty() {
        eprintln!(
            "❌ The specified audio file was not found: {:?}",
            cue_sheet.audio_file_path
        );
        std::process::exit(1);
    };

    let best_match = audio_files_in_same_dir
        .iter()
        .min_by_key(|(_, distance)| *distance)
        .unwrap();

    let best_match_file_name = best_match.0.file_name().unwrap();
    // transform distance to success rate (0 = no match, 100 = perfect match)
    println!("{:?}", best_match);
    let success_rate = 100 - (best_match.1 * 100 / broken_file_name.len());

    // Ask user if this is ok
    println!("🔧 Found a similar audio file in the same directory:",);
    println!(
        "\t{:?} -> {:?} ({}%)",
        cue_sheet.audio_file_path.file_name().unwrap(),
        best_match_file_name,
        success_rate
    );
    println!("🔧 Do you want to use this file instead? (Y/n)");

    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();
    if !input.trim().is_empty() && !input.trim().eq_ignore_ascii_case("y") {
        println!("🚪 Exiting ...");
        std::process::exit(0);
    }

    println!(
        "✅ Fixed audio file path case: {:?} -> {:?}",
        cue_sheet.audio_file_path.file_name().unwrap(),
        best_match_file_name
    );
    cue_sheet.audio_file_path = best_match.0.clone();
}

/// Finds all cue files in the given input path
/// The input path could be:
/// - A single file
/// - A directory
/// - A list of files and directories
fn find_cue_files(input_path: &Path) -> Vec<PathBuf> {
    let mut cue_files = Vec::new();

    if input_path.is_file() {
        if input_path
            .extension()
            .unwrap_or("".as_ref())
            .eq_ignore_ascii_case("cue")
        {
            cue_files.push(input_path.to_path_buf());
        }
    } else if input_path.is_dir() {
        for dir_entry in input_path.read_dir().unwrap() {
            if dir_entry.is_err() {
                continue;
            }
            let dir_entry = dir_entry.unwrap();
            let path = dir_entry.path();
            cue_files.extend(find_cue_files(&path));
        }
    }

    cue_files
}

/// Tests if the given commands are available in the users path
/// The test is done by using the `which` command
fn check_tools(commands: Vec<&str>) {
    for command in commands {
        let output = Command::new("which")
            .arg(command)
            .output()
            .expect("Failed to execute which command");

        if !output.status.success() {
            eprintln!("Command {} not found in path", command);
            std::process::exit(1);
        }
    }
}

fn build_ffmpeg_commands(cue_sheet: &CueSheet) -> Vec<Track> {
    cue_sheet
        .tracks
        .iter()
        .enumerate()
        .map(|(index, track)| build_ffmpeg_command(cue_sheet, index, track))
        .collect()
}

fn build_ffmpeg_command(cue_sheet: &CueSheet, index: usize, track: &Track) -> Track {
    let cue_duration = track.start_time.as_ref().unwrap();

    // Convert frames to milliseconds (1 CDDA frame = 1/75 second)
    let milliseconds = cue_duration.frames * 1000 / 75;

    // Convert minutes to hours and remaining minutes
    let hours = cue_duration.minutes / 60;
    let minutes = cue_duration.minutes % 60;

    // Format as "hh:mm:ss.mmm"
    let ffmpeg_start_time = format!(
        "{:02}:{:02}:{:02}.{:03}",
        hours, minutes, cue_duration.seconds, milliseconds
    );

    // Calculate the end time based on the next track, if we have the last track, skip this param
    // let ffmpeg_end_time_param = format!("-to {:02}:{:02}:{:02}.{:03}", hours, minutes, cue_duration.seconds + 30, milliseconds);
    let ffmpeg_end_time = if index < cue_sheet.tracks.len() - 1 {
        let next_track = &cue_sheet.tracks[index + 1];
        let next_cue_duration = next_track.start_time.as_ref().unwrap();
        let next_milliseconds = next_cue_duration.frames * 1000 / 75;
        let next_hours = next_cue_duration.minutes / 60;
        let next_minutes = next_cue_duration.minutes % 60;
        format!(
            "-to \"{:02}:{:02}:{:02}.{:03}\"",
            next_hours, next_minutes, next_cue_duration.seconds, next_milliseconds
        )
    } else {
        "".to_string()
    };

    let output_file_name = build_output_name(cue_sheet, track);

    let audio_file_path = cue_sheet.audio_file_path.to_str().unwrap();

    let command = format!(
        "ffmpeg -i \"{}\" -acodec copy -ss \"{}\" {} \"{}\"",
        audio_file_path, ffmpeg_start_time, ffmpeg_end_time, output_file_name
    );

    Track {
        number: track.number,
        title: track.title.clone(),
        artist: track.artist.clone(),
        start_time: track.start_time,
        output_file: Some(PathBuf::from(output_file_name)),
        ffmpeg_command: Some(command),
    }
}

fn build_output_name(cue_sheet: &CueSheet, track: &Track) -> String {
    let extension = cue_sheet.file_name.split('.').last().unwrap_or_else(|| {
        panic!(
            "❌ Could not determine extension for file {}",
            cue_sheet.file_name
        )
    });

    // Create a sub dir for each cue file
    let sub_dir = cue_sheet
        .audio_file_path
        .parent()
        .unwrap()
        .join(cue_sheet.file_name.split('.').next().unwrap());
    let sub_dir = sub_dir.to_str().unwrap();

    // Create a filename for each track
    let track_number = format!("{:02}", track.number);
    let track_title = if let Some(ref title) = track.title {
        if let Some(ref artist) = track.artist {
            format!("{} - {}", artist, title)
        } else {
            title.to_string()
        }
    } else {
        "Unknown".to_string()
    };
    let filename = format!("{} {}", track_number, track_title);

    format!("{}/{}.{}", sub_dir, filename, extension)
}

fn parse_cue_file(cue_file_path: &PathBuf) -> Option<CueSheet> {
    println!("📖 Parsing cue file \"{}\"", cue_file_path.display());

    let file = File::open(cue_file_path).unwrap();

    let mut file_name = String::new();
    let mut tracks = Vec::new();
    let mut current_track = None;

    let cue_file_content = read_cue_file_content(cue_file_path, file);

    for line in cue_file_content.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();

        if tokens.is_empty() {
            continue;
        }

        match tokens[0] {
            "FILE" => {
                file_name = tokens[1].replace("\"", "").to_string();
            }
            "TRACK" => {
                if let Some(track) = current_track.take() {
                    tracks.push(track);
                }

                let track_number = tokens[1].parse::<u32>().unwrap_or(0);
                current_track = Some(Track {
                    number: track_number,
                    title: None,
                    start_time: None,
                    artist: None,
                    output_file: None,
                    ffmpeg_command: None,
                });
            }
            "TITLE" => {
                if let Some(ref mut track) = current_track {
                    track.title = Some(tokens[1..].join(" ").replace("\"", ""));
                }
            }
            "INDEX" => {
                if tokens[1] == "01" {
                    if let Some(ref mut track) = current_track {
                        let start_time = tokens[2].to_string();
                        let time_split = start_time.split(":").collect::<Vec<&str>>();
                        track.start_time = Some(CueDuration {
                            minutes: time_split[0].parse::<u32>().unwrap(),
                            seconds: time_split[1].parse::<u32>().unwrap(),
                            frames: time_split[2].parse::<u32>().unwrap(),
                        });
                    }
                }
            }
            "PERFORMER" => {
                if let Some(ref mut track) = current_track {
                    track.artist = Some(tokens[1..].join(" ").replace("\"", ""));
                }
            }
            _ => {}
        }
    }

    if let Some(track) = current_track {
        tracks.push(track);
    }

    Some(CueSheet {
        file_name: file_name.clone(),
        cue_file_path: cue_file_path.to_path_buf(),
        audio_file_path: cue_file_path
            .to_path_buf()
            .parent()
            .unwrap()
            .join(file_name),
        tracks,
    })
}

fn read_cue_file_content(cue_file_path: &Path, file: File) -> String {
    // Read file content
    let mut data_buffer: Vec<u8> = Vec::new();
    let mut cue_file = BufReader::new(file);
    cue_file.read_to_end(&mut data_buffer).unwrap();

    // Detect encoding and convert to utf8
    let detected_encoding = chardet::detect(&data_buffer);
    let encoding_ref =
        encoding::label::encoding_from_whatwg_label(charset2encoding(&detected_encoding.0));
    if let Some(encoding_ref) = encoding_ref {
        encoding_ref
            .decode(&data_buffer, DecoderTrap::Ignore)
            .unwrap_or_else(|_| {
                panic!(
                    "{}",
                    format!("❌ Could not decode cue file {}", cue_file_path.display())
                )
            })
    } else {
        panic!(
            "{}",
            format!("❌ Could not decode cue file {}", cue_file_path.display())
        );
    }
}
