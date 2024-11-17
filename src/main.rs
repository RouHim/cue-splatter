mod updater;

use argh::FromArgs;
use chardet::charset2encoding;
use colour::{blue_ln, green_ln, red_ln, yellow_ln};
use encoding::DecoderTrap;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use lazy_static::lazy_static;
use lofty::config::WriteOptions;
use lofty::file::TaggedFileExt;
use lofty::tag::{Accessor, Tag, TagExt};
use rayon::iter::{IntoParallelRefIterator, ParallelBridge, ParallelIterator};
use std::cmp::{Ordering, PartialEq, PartialOrd};
use std::fs::{DirEntry, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::RwLock;
use std::time::Duration;

#[derive(Debug, FromArgs)]
/// Split audio files based on cue sheets
struct CliArgs {
    /// only print the ffmpeg commands
    #[argh(switch)]
    dry_run: bool,

    /// move the audio file to the output dir
    #[argh(switch)]
    transfer: bool,

    /// file or folder paths to parse
    /// default is "."
    #[argh(positional, greedy)]
    cue_file_or_folders: Vec<String>,
}

#[derive(Debug, Clone)]
struct CueSheet {
    cue_file_path: PathBuf,
    audio_file_path: PathBuf,
    audio_file_name: String,
    output_dir: Option<PathBuf>,
    title: Option<String>,
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

#[derive(Debug, Default, Copy, Clone, PartialEq)]
struct CueDuration {
    minutes: u32,
    seconds: u32,
    frames: u32,
}

#[derive(Debug, Clone, PartialOrd, PartialEq)]
enum UserDefaultAction {
    Yes,
    No,
}

impl PartialOrd for CueDuration {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        let self_frames = self.minutes * 60 * 75 + self.seconds * 75 + self.frames;
        let other_frames = other.minutes * 60 * 75 + other.seconds * 75 + other.frames;
        Some(self_frames.cmp(&other_frames))
    }
}

fn main() {
    let mut cli_args: CliArgs = argh::from_env();
    if cli_args.cue_file_or_folders.is_empty() {
        cli_args.cue_file_or_folders.push(".".to_string());
    }

    // Check for updates, if available, update the binary and restart
    updater::update();

    check_tools(vec!["ffmpeg", "ffprobe"]);

    // Find cue files
    let cue_file_paths = cli_args
        .cue_file_or_folders
        .iter()
        .flat_map(|input_path| find_cue_files(Path::new(input_path)))
        .collect();

    // Show cue files to user
    let_user_verify_cue_files(&cue_file_paths);

    // Verify cue files
    let cue_sheets: Vec<CueSheet> = cue_file_paths
        .iter()
        .flat_map(parse_cue_file)
        .map(|mut cue_sheet| {
            let fix_action = verify_cue_files(&mut cue_sheet);
            match fix_action {
                FixAction::Deleted => return None,
                FixAction::Modified => {}
                FixAction::None => {}
            }

            augment_with_ffmpeg_commands(&mut cue_sheet);
            augment_with_output_dir(&mut cue_sheet);
            Some(cue_sheet)
        })
        .flat_map(|cue_sheet| cue_sheet)
        .collect();

    if cli_args.dry_run {
        println!("🚀 Dry run, only printing ffmpeg commands");
        for cue_sheet in &cue_sheets {
            for track in &cue_sheet.tracks {
                println!("{}", track.ffmpeg_command.as_ref().unwrap());
            }
        }
    } else {
        println!();

        // Split tracks and write metadata
        let failed_tracks = run_ffmpeg_split_commands(&cue_sheets);
        if failed_tracks.is_empty() {
            println!("🎉 All tracks have been splitted");

            // Moves the audio file to the output dir
            if cli_args.transfer {
                move_input_audio_files(cue_sheets);
            }
        } else {
            report_failed_tracks(failed_tracks);
        }
    }

    println!("🚪 Everything is done, bye bye");
}

fn augment_with_output_dir(cue_sheet: &mut CueSheet) {
    let first_track = cue_sheet.tracks.first().unwrap();
    let output_dir = first_track.output_file.as_ref().unwrap().parent().unwrap();
    cue_sheet.output_dir = Some(output_dir.to_path_buf());
}

fn write_audio_metadata_to_track(cue_sheet: &CueSheet, track: &Track) -> (bool, String) {
    let output_file_path = track.output_file.as_ref().unwrap();
    let tagged_file = lofty::read_from_path(output_file_path);
    if tagged_file.is_err() {
        return (
            false,
            format!(
                "❌ Could not read file {}\n{}",
                output_file_path.display(),
                tagged_file.err().unwrap()
            ),
        );
    }
    let mut tagged_file = tagged_file.unwrap();

    let primary_tag = match tagged_file.primary_tag_mut() {
        Some(primary_tag) => primary_tag,
        None => {
            if let Some(first_tag) = tagged_file.first_tag_mut() {
                first_tag
            } else {
                let tag_type = tagged_file.primary_tag_type();
                tagged_file.insert_tag(Tag::new(tag_type));
                tagged_file.primary_tag_mut().unwrap()
            }
        }
    };

    primary_tag.set_track(track.number);
    if let Some(ref album) = cue_sheet.title {
        primary_tag.set_album(album.to_string());
    }
    if let Some(ref title) = track.title {
        primary_tag.set_title(title.to_string());
    }
    if let Some(ref artist) = track.artist {
        primary_tag.set_artist(artist.to_string());
    }

    primary_tag
        .save_to_path(output_file_path, WriteOptions::default())
        .unwrap();

    (true, "".to_string())
}

fn move_input_audio_files(cue_sheets: Vec<CueSheet>) {
    println!("⏩  Moving audio files to output directories");
    for cue_file in cue_sheets {
        // Move the audio file to the output dir (only if it is not identical)
        let audio_dir = cue_file.audio_file_path.parent().unwrap();
        let output_dir = cue_file.output_dir.unwrap();

        if audio_dir == output_dir {
            println!("📦 Audio file is already in the output directory");
        } else {
            let audio_file_name = cue_file.audio_file_path.file_name().unwrap();
            let output_audio_file = output_dir.join(audio_file_name);
            std::fs::rename(&cue_file.audio_file_path, &output_audio_file).unwrap();
            println!("📦 Moved audio file to: {}", output_audio_file.display());
        }
    }
    println!("🎉 All files have been moved");
}

fn let_user_verify_cue_files(cue_files: &Vec<PathBuf>) {
    println!("Found {} cue file(s):", cue_files.len());
    for cue_file in cue_files {
        println!("\t{}", cue_file.display());
    }
    println!();
    blue_ln!("Proceed with splitting? (Y/n): ");

    // proceed if user enters y|Y or just hits ENTER
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();
    if !input.trim().is_empty() && !input.trim().eq_ignore_ascii_case("y") {
        println!("🚪 Exiting ...");
        std::process::exit(0);
    }
}

fn report_failed_tracks(failed_tracks: Vec<(Track, String)>) {
    println!("❌ Failed to split the following tracks:");
    println!();
    for (track, error_message) in failed_tracks {
        println!("\tArtist: {:?}", track.artist);
        println!("\tTitle: {:?}", track.title);
        println!("\tCommand: {}", track.ffmpeg_command.unwrap());
        println!("\tOutput file: {}", track.output_file.unwrap().display());
        println!("\tError message: {}", error_message);
        println!();
        println!();
        println!();
    }
}

fn run_ffmpeg_split_commands(cue_sheets: &[CueSheet]) -> Vec<(Track, String)> {
    let total_track_count = cue_sheets
        .iter()
        .map(|cue_sheet| cue_sheet.tracks.len() as u64)
        .sum();

    let multi_progress_bar = MultiProgress::new();
    let mp_progress_bar = multi_progress_bar.add(
        ProgressBar::new(total_track_count)
            .with_style(
                ProgressStyle::default_bar()
                    .template("{msg}: {pos}/{len}")
                    .unwrap(),
            )
            .with_message("✂️ Splitting tracks"),
    );
    mp_progress_bar.enable_steady_tick(Duration::from_millis(100));

    // Collect failed tracks in a vec
    let failed_tracks: RwLock<Vec<(Track, String)>> = RwLock::new(Vec::new());

    cue_sheets
        .iter()
        .flat_map(|cue_sheet| cue_sheet.tracks.iter().map(move |track| (cue_sheet, track)))
        .par_bridge()
        .for_each(|(cue_sheet, track)| {
            split_track(&multi_progress_bar, &failed_tracks, cue_sheet, track);
            mp_progress_bar.inc(1);
        });

    mp_progress_bar.finish_and_clear();

    failed_tracks.into_inner().unwrap()
}

fn split_track(
    multi_progress_bar: &MultiProgress,
    failed_tracks: &RwLock<Vec<(Track, String)>>,
    cue_sheet: &CueSheet,
    track: &Track,
) {
    let split_command_bar = create_spinner(multi_progress_bar, track);

    // Run ffmpeg split command
    let (is_ok, error_message) = run_ffmpeg_split_command(track);

    if is_ok {
        // Write metadata to track
        let (is_ok, error_message) = write_audio_metadata_to_track(cue_sheet, track);

        if !is_ok {
            failed_tracks
                .write()
                .unwrap()
                .push((track.clone(), error_message));
        }
    } else {
        failed_tracks
            .write()
            .unwrap()
            .push((track.clone(), error_message));
    }

    split_command_bar.finish_and_clear();
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

/// Runs the ffmpeg command to split the audio file
/// Returns if the command was successful and the error message
fn run_ffmpeg_split_command(track: &Track) -> (bool, String) {
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

    if output.status.success() {
        (true, "".to_string())
    } else {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        (false, format!("{}\n{}", stdout, stderr))
    }
}

fn verify_cue_files(cue_sheet: &mut CueSheet) -> FixAction {
    println!("🔍 Verifying cue file",);

    // Verify that the cue file exists
    if !cue_sheet.cue_file_path.exists() {
        eprintln!(
            "❌ The cue sheet's audio file was not found: {:?}",
            cue_sheet.cue_file_path
        );
        std::process::exit(1);
    }

    // Verify that the audio file name exists
    if !cue_sheet.audio_file_path.exists() {
        yellow_ln!(
            "❌ The referenced audio file of the cue sheet was not found: {:?}",
            cue_sheet.audio_file_path
        );
        match fix_cue_sheet_audio_file_reference(cue_sheet) {
            FixAction::Modified => {
                return verify_cue_files(cue_sheet);
            }
            FixAction::Deleted => {
                return FixAction::Deleted;
            }
            FixAction::None => {}
        }
    };

    // Verify that there are tracks in the cue file
    if cue_sheet.tracks.is_empty() {
        eprintln!(
            "❌ No tracks found in cue file {}",
            cue_sheet.audio_file_name
        );
        std::process::exit(1);
    }

    // Verify that ffmpeg can process the input file
    // Example: ffprobe -v error -select_streams a:0 -count_packets -show_entries stream=codec_type,codec_name -of csv=p=0 input_file.mp3
    let ffprobe_cmd = format!(
        "ffprobe -v error -select_streams a:0 -count_packets -show_entries stream=codec_type,codec_name -of csv=p=0 \"{}\"",
        cue_sheet.audio_file_path.display()
    );
    let output = Command::new("sh")
        .arg("-c")
        .arg(ffprobe_cmd)
        .output()
        .expect("Failed to execute command");
    if !output.status.success() {
        eprintln!(
            "❌ ffmpeg failed to process file, most likely the file is corrupt or codec is not supported: {}\nstdout: {}\nstderr: {}",
            cue_sheet.audio_file_name,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        std::process::exit(1);
    }

    // Verify that all tracks have a start time
    for track in &cue_sheet.tracks {
        if track.start_time.is_none() {
            eprintln!("❌ No start time found for track {}", track.number);
            std::process::exit(1);
        }
    }

    // Verify that track start time is strictly monotonic growing
    for (i, track) in cue_sheet.tracks.iter().enumerate() {
        if i > 0 {
            let previous_track = &cue_sheet.tracks[i - 1];
            if track.start_time.unwrap() <= previous_track.start_time.unwrap() {
                eprintln!("❌ Track start time is not strictly monotonic growing: Track {} starts at {:?}, but previous track starts at {:?}", track.number, track.start_time.unwrap(), previous_track.start_time.unwrap());
                eprintln!(
                    "❌ Most likely the cue file is not valid: \"{}\"",
                    cue_sheet.cue_file_path.display()
                );
                std::process::exit(1);
            }
        }
    }

    println!("✅ Cue file is valid");
    println!();

    FixAction::None
}

enum FixAction {
    /// The cue file was modified by the fix action, we should retry the process
    Modified,
    /// The cue file was deleted by the fix action, we should skip the process
    Deleted,
    /// The cue file was not modified by the fix action, nothing special to do
    None,
}

/// Lets the user fix the cue file
fn ask_user_for_fix(cue_sheet: &mut CueSheet) -> FixAction {
    blue_ln!("🔧 What do you want to do with the cure file? (e)dit, (d)elete, (l)ist files, (v)iew, (r)etry, (q)uit: ");

    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();

    match input.trim() {
        "e" => {
            let editor = std::env::var("EDITOR").unwrap_or("nano".to_string());
            Command::new(editor)
                .arg(cue_sheet.cue_file_path.as_os_str())
                .status()
                .expect("Failed to open file in editor");

            ask_user_for_fix(cue_sheet)
        }
        "d" => {
            std::fs::remove_file(&cue_sheet.cue_file_path).unwrap();
            println!("🗑 Deleted cue file: {:?}", cue_sheet.cue_file_path);

            FixAction::Deleted
        }
        "v" => {
            Command::new("open")
                .arg(cue_sheet.cue_file_path.as_os_str())
                .status()
                .expect("Failed to open file in viewer");

            ask_user_for_fix(cue_sheet)
        }
        "l" => {
            let parent_dir = cue_sheet.audio_file_path.parent().unwrap();
            let files_in_directory: Vec<DirEntry> = parent_dir
                .read_dir()
                .unwrap()
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.path().is_file())
                .collect();
            for entry in files_in_directory {
                println!("\t - {}", entry.path().display());
            }

            ask_user_for_fix(cue_sheet)
        }
        "r" => FixAction::Modified,
        "q" => {
            println!("🚪 Exiting ...");
            std::process::exit(1);
        }
        _ => {
            println!("Invalid input, please try again");
            ask_user_for_fix(cue_sheet)
        }
    }
}

/// Fixes the audio file reference in the cue sheet
/// This happens e.g. when the case of the audio file path in the cue sheet does not match the actual file path
/// This is a common issue on Windows file systems
fn fix_cue_sheet_audio_file_reference(cue_sheet: &mut CueSheet) -> FixAction {
    let broken_file_name = cue_sheet
        .audio_file_path
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    let parent_dir = cue_sheet.audio_file_path.parent().unwrap();

    let best_match = find_best_match(cue_sheet, parent_dir, broken_file_name);
    if best_match.is_none() {
        return ask_user_for_fix(cue_sheet);
    }
    let best_match = best_match.unwrap();
    let best_match_file_name = best_match.0.file_name().unwrap();

    // Ask user if this is ok
    println!("🔧 Found a similar audio file in the same directory:",);
    let score = best_match.1;

    println!(
        "\t{:?} -> {:?} ({}%)",
        cue_sheet.audio_file_path.file_name().unwrap(),
        best_match_file_name,
        score
    );

    let default_action: UserDefaultAction = if score > 85 {
        green_ln!("🔧 Do you want to use this file instead? (Y/n): ");
        UserDefaultAction::Yes
    } else if score > 70 {
        yellow_ln!("🔧 Do you want to use this file instead? (Y/n): ");
        UserDefaultAction::Yes
    } else {
        red_ln!("🔧 Do you want to use this file instead? (y/N): ");
        UserDefaultAction::No
    };

    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();

    let cancel_process = match input.trim() {
        "y" | "Y" => false,
        "n" | "N" => true,
        _ => default_action == UserDefaultAction::No,
    };

    if cancel_process {
        red_ln!("❌ The referenced audio could not be fixed automatically");
        return ask_user_for_fix(cue_sheet);
    }

    println!(
        "✅ Fixed audio file path case: {:?} -> {:?}",
        cue_sheet.audio_file_path.file_name().unwrap(),
        best_match_file_name
    );
    cue_sheet.audio_file_path = best_match.0.clone();

    FixAction::None
}

/// Finds the best match for the audio file in the same directory
fn find_best_match(
    cue_sheet: &CueSheet,
    parent_dir: &Path,
    broken_file_name: &str,
) -> Option<(PathBuf, usize)> {
    // Find all entries in the parent directory
    let files_in_directory: Vec<DirEntry> = parent_dir
        .read_dir()
        .unwrap()
        .filter_map(|entry| entry.ok())
        .collect();

    // Then find all valid audio files in the directory
    let audio_files_in_directory: Vec<PathBuf> = files_in_directory
        .par_iter()
        .filter(|entry| entry.file_type().unwrap().is_file())
        .filter(|entry| audio_playtime_matches(entry, cue_sheet.tracks.last().unwrap()))
        .map(|entry| entry.path())
        .collect();

    if audio_files_in_directory.is_empty() {
        eprintln!(
            "❌ The referenced audio file of the cue sheet was not found: {:?}",
            cue_sheet.audio_file_path
        );
        std::process::exit(1);
    };

    // Calculate the levenshtein distance between the broken file name and the actual file name
    let levenshtein_result =
        find_best_levenshtein_match(broken_file_name, &audio_files_in_directory);

    // Calculate the hamming distance between the broken file name and the actual file name
    let hamming_result = find_best_hamming_match(broken_file_name, &audio_files_in_directory);

    // If both are present, take the one with the better score
    // If they are equal, take the levenshtein result
    if let Some(levenshtein_result) = &levenshtein_result {
        if let Some(hamming_result) = &hamming_result {
            if levenshtein_result.0 == hamming_result.0 {
                return Some(levenshtein_result.clone());
            }
            // If they differ, take the one with the better score
            else if levenshtein_result.1 > hamming_result.1 {
                return Some(levenshtein_result.clone());
            } else {
                return Some(hamming_result.clone());
            }
        }
    }

    // If one is missing but the other is present, return the present one
    if levenshtein_result.is_some() && hamming_result.is_none() {
        return Some(levenshtein_result.unwrap());
    } else if levenshtein_result.is_none() && hamming_result.is_some() {
        return Some(hamming_result.unwrap());
    }

    yellow_ln!(
        "❌ Could not find a good match for the audio file in the same directory: {:?}",
        cue_sheet.audio_file_path
    );

    None
}

fn audio_playtime_matches(entry: &DirEntry, last_track: &Track) -> bool {
    let mut matches = false;
    if let Some(entry_playtime) = read_audio_playtime(entry) {
        let last_track_start = last_track.start_time.unwrap();
        let last_track_start_seconds = last_track_start.minutes * 60 + last_track_start.seconds;
        matches = entry_playtime >= last_track_start_seconds
    }
    matches
}

/// Read the length of the audio file using ffprobe
/// Returns the length in seconds
/// Example call: ffprobe -v error -show_entries format=duration -of default=noprint_wrappers=1:nokey=1 input.mp3
fn read_audio_playtime(entry: &DirEntry) -> Option<u32> {
    // Build ffprobe command
    let ffprobe_command = format!(
        "ffprobe -v error -show_entries format=duration -of default=noprint_wrappers=1:nokey=1 \"{}\"",
        entry.path().display()
    );

    // Run ffprobe command
    let output = Command::new("sh")
        .arg("-c")
        .arg(ffprobe_command)
        .output()
        .expect("Failed to execute command");

    // Check if ffprobe command was successful
    if !output.status.success() {
        return None;
    }

    // Parse ffprobe output
    let output = String::from_utf8_lossy(&output.stdout);
    if let Ok(playtime) = output.trim().parse::<f32>() {
        Some(playtime as u32)
    } else {
        None
    }
}

fn find_best_hamming_match(
    broken_file_name: &str,
    audio_files_in_same_dir: &[PathBuf],
) -> Option<(PathBuf, usize)> {
    let audio_files_ham: Vec<(PathBuf, usize)> = audio_files_in_same_dir
        .iter()
        .map(|audio_entry| {
            let entry_file_name = audio_entry.file_name();
            let entry_file_name = entry_file_name.unwrap().to_str().unwrap();

            // Remove extension
            let entry_file_name = entry_file_name.split('.').next().unwrap();
            let broken_file_name = broken_file_name.split('.').next().unwrap();

            (
                audio_entry.clone(),
                hamming_distance(entry_file_name.as_bytes(), broken_file_name.as_bytes()),
            )
        })
        .collect();

    // If we have multiple entries with the same distance, we can't determine the best match
    // In this case, we return None
    let all_have_same_distance = audio_files_ham
        .iter()
        .all(|(_, distance)| *distance == audio_files_ham[0].1);
    if audio_files_ham.len() > 1 && all_have_same_distance {
        return None;
    }

    // Find the best match (smallest distance)
    let best_match = audio_files_ham.iter().min_by(|a, b| a.1.cmp(&b.1)).unwrap();

    // Calculate the success rate
    let best_match_file_name = best_match.0.file_name().unwrap().to_str().unwrap();

    // Remove extension
    let best_match_file_name = best_match_file_name.split('.').next().unwrap();
    let broken_file_name = broken_file_name.split('.').next().unwrap();

    let hamming_distance = best_match.1;
    let shortest_length = size_of_shortest(broken_file_name, best_match_file_name);

    let success_rate = 100 - (hamming_distance * 100 / shortest_length);

    Some((best_match.0.clone(), success_rate))
}

fn hamming_distance(x: &[u8], y: &[u8]) -> usize {
    x.iter().zip(y.iter()).filter(|(a, b)| a != b).count()
}

fn find_best_levenshtein_match(
    broken_file_name: &str,
    audio_files_in_same_dir: &[PathBuf],
) -> Option<(PathBuf, usize)> {
    let audio_files_lev: Vec<(PathBuf, usize)> = audio_files_in_same_dir
        .iter()
        .map(|audio_entry| {
            let entry_file_name = audio_entry.file_name();
            let entry_file_name = entry_file_name.unwrap().to_str().unwrap();

            // Remove extension
            let entry_file_name = entry_file_name.split('.').next().unwrap();
            let broken_file_name = broken_file_name.split('.').next().unwrap();

            (
                audio_entry.clone(),
                levenshtein::levenshtein(broken_file_name, entry_file_name),
            )
        })
        .collect();

    // If we have multiple entries with the same distance, we can't determine the best match
    // In this case, we return None
    let all_have_same_distance = audio_files_lev
        .iter()
        .all(|(_, distance)| *distance == audio_files_lev[0].1);
    if audio_files_lev.len() > 1 && all_have_same_distance {
        return None;
    }

    // Find the best match (smallest distance)
    let best_match = audio_files_lev.iter().min_by(|a, b| a.1.cmp(&b.1)).unwrap();

    // Calculate the success rate
    let file_name_length = size_of_longest(
        broken_file_name,
        best_match.0.file_name().unwrap().to_str().unwrap(),
    );
    let levenshtein_distance = best_match.1;
    let success_rate = 100 - (levenshtein_distance * 100 / file_name_length);

    Some((best_match.0.clone(), success_rate))
}

// Returns the length of the longest string
fn size_of_longest(a: &str, b: &str) -> usize {
    if a.len() > b.len() {
        a.len()
    } else {
        b.len()
    }
}

// Returns the length of the shortest string
fn size_of_shortest(a: &str, b: &str) -> usize {
    if a.len() < b.len() {
        a.len()
    } else {
        b.len()
    }
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

fn augment_with_ffmpeg_commands(cue_sheet: &mut CueSheet) {
    let augmented_tracks: Vec<Track> = cue_sheet
        .tracks
        .iter()
        .enumerate()
        .map(|(index, track)| build_ffmpeg_command(cue_sheet, index, track))
        .collect();

    cue_sheet.tracks = augmented_tracks;
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
        "ffmpeg -y -i \"{}\" -map_metadata -1 -acodec copy -ss \"{}\" {} \"{}\"",
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
    let extension = cue_sheet
        .audio_file_name
        .split('.')
        .last()
        .unwrap_or_else(|| {
            eprintln!(
                "❌ Could not determine extension for file {}",
                cue_sheet.audio_file_name
            );
            std::process::exit(1);
        });

    // Create a sub dir for each cue file
    let sub_dir_name = if is_multi_disc(cue_sheet) {
        let disk_number = derive_disk_number(&cue_sheet.cue_file_path);
        format!("CD{}", disk_number)
    } else {
        ".".to_string()
    };
    let sub_dir = cue_sheet
        .audio_file_path
        .parent()
        .unwrap()
        .join(sub_dir_name);
    let sub_dir = sub_dir.to_str().unwrap();

    // Create a filename for each track
    let track_number = format!("{:02}", track.number);
    let mut track_title = if let Some(ref title) = track.title {
        if let Some(ref artist) = track.artist {
            format!("{} - {}", artist, title)
        } else {
            title.to_string()
        }
    } else {
        "Unknown".to_string()
    };

    // Replace invalid characters
    track_title = track_title
        .replace("/", "-")
        .replace("\\", "-")
        .replace(":", "-")
        .replace("`", "'")
        .trim()
        .to_string();

    let filename = format!("{} {}", track_number, track_title);

    format!("{}/{}.{}", sub_dir, filename, extension)
}

/// Derives the disk number from the cue file path
/// This is done by checking the file name for common patterns
fn derive_disk_number(cue_file_path: &Path) -> usize {
    // First split the cue file name by common delimiters
    let cue_file_name = cue_file_path.file_name().unwrap().to_str().unwrap();
    let delimiter = find_best_delimiter(cue_file_name);
    let tokens: Vec<&str> = cue_file_name.split(&delimiter).collect();

    // Check if there is a token that starts trimmed ignoring case with CD
    let mut disk_number = tokens
        .iter()
        .filter_map(|token| {
            if token.trim().to_lowercase().starts_with("cd") {
                token
                    .trim()
                    .to_lowercase()
                    .strip_prefix("cd")
                    .unwrap()
                    .parse::<usize>()
                    .ok()
            } else {
                None
            }
        })
        .next();

    // Check if there is a token that starts trimmed ignoring case with disc
    if disk_number.is_none() {
        disk_number = tokens
            .iter()
            .filter_map(|token| {
                if token.trim().to_lowercase().starts_with("disc") {
                    token
                        .trim()
                        .to_lowercase()
                        .strip_prefix("disc")
                        .unwrap()
                        .parse::<usize>()
                        .ok()
                } else {
                    None
                }
            })
            .next();
    }

    // If we still have no disk number check the first token, if it is a number
    if disk_number.is_none() {
        // Strip 0 chars from string, and take only the first char
        let first_token = tokens[0]
            .replace("0", "")
            .trim()
            .chars()
            .next()
            .unwrap()
            .to_string();
        if let Ok(number) = first_token.parse::<usize>() {
            disk_number = Some(number);
        };
    };

    // If we have still no disk number check if the cue file name contains a number
    if disk_number.is_none() {
        let number = cue_file_name
            .chars()
            .filter(|c| c.is_numeric())
            .collect::<String>();
        if let Ok(number) = number.parse::<usize>() {
            disk_number = Some(number);
        }
    }

    // as default, use 1
    disk_number.unwrap_or(1)
}

/// Finds the best delimiter in the file name
fn find_best_delimiter(file_name: &str) -> String {
    lazy_static! {
        static ref DELIMITERS: Vec<&'static str> = vec!["-", "_", " ", "."];
    }

    let mut best_delimiter = "";
    let mut best_delimiter_score = 0;

    for delimiter in DELIMITERS.iter() {
        let score = file_name.matches(delimiter).count();
        if score > best_delimiter_score {
            best_delimiter = delimiter;
            best_delimiter_score = score;
        }
    }

    best_delimiter.to_string()
}

/// Determines if the current release is a multidisc release
/// This is done by checking if there are multiple cue files in the same directory
/// If there are multiple cue files, the release is considered a multidisc release
fn is_multi_disc(cue_sheet: &CueSheet) -> bool {
    let parent_dir = cue_sheet.cue_file_path.parent().unwrap();
    let cue_files_in_directory: Vec<DirEntry> = parent_dir
        .read_dir()
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .filter(|entry| {
            entry
                .path()
                .extension()
                .unwrap_or("".as_ref())
                .eq_ignore_ascii_case("cue")
        })
        .collect();

    cue_files_in_directory.len() > 1
}

fn parse_cue_file(cue_file_path: &PathBuf) -> Option<CueSheet> {
    println!();
    println!("{}", cue_file_path.display());
    println!("============================================================");
    println!("📖 Parsing cue file");

    let file = File::open(cue_file_path).unwrap();
    let cue_file_content = read_cue_file_content(cue_file_path, file);

    let mut audio_file_name = String::new();
    let mut tracks = Vec::new();
    let mut current_track = None;
    let mut title = None;

    for line in cue_file_content.lines() {
        let line_split = line.trim().split_once(' ').unwrap_or(("", ""));
        let cue_line_key = line_split.0;
        let cue_line_value = line_split.1;

        if cue_line_key.is_empty() {
            continue;
        }

        match cue_line_key {
            "FILE" => {
                // Take only the value in quotes
                let first_index_of_quote = cue_line_value.find('\"').unwrap();
                let last_index_of_quote = cue_line_value.rfind('\"').unwrap();
                audio_file_name =
                    cue_line_value[first_index_of_quote + 1..last_index_of_quote].to_string();
            }
            "TRACK" => {
                if let Some(track) = current_track.take() {
                    tracks.push(track);
                }

                let track_number: u32 = cue_line_value
                    .split_whitespace()
                    .next()
                    .unwrap()
                    .parse()
                    .unwrap();
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
                    track.title = Some(cue_line_value.replace("\"", "").trim().to_string());
                } else {
                    title = Some(cue_line_value.replace("\"", "").trim().to_string());
                }
            }
            "INDEX" => {
                if let Some(ref mut track) = current_track {
                    let cue_duration = cue_line_value.split(' ').last().unwrap();
                    let cue_duration_split: Vec<&str> = cue_duration.split(':').collect();
                    let minutes = cue_duration_split[0].parse::<u32>().unwrap();
                    let seconds = cue_duration_split[1].parse::<u32>().unwrap();
                    let frames = cue_duration_split[2].parse::<u32>().unwrap();
                    track.start_time = Some(CueDuration {
                        minutes,
                        seconds,
                        frames,
                    });
                }
            }
            "PERFORMER" => {
                if let Some(ref mut track) = current_track {
                    track.artist = Some(cue_line_value.replace("\"", "").trim().to_string());
                }
            }
            _ => {}
        }
    }

    if let Some(track) = current_track {
        tracks.push(track);
    }

    println!("🎵 Found {} track(s)", tracks.len());

    Some(CueSheet {
        audio_file_name: audio_file_name.clone(),
        cue_file_path: cue_file_path.to_path_buf(),
        title,
        audio_file_path: cue_file_path
            .to_path_buf()
            .parent()
            .unwrap()
            .join(audio_file_name),
        tracks,
        output_dir: None,
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
