use std::fs::File;
use std::io::{BufRead, BufReader};

fn main() {
    // Read cue file
    let cue_file_path = "test-data/va_-_tuning_beats_2003_volume_2-twc.cue";
    let cue_sheet = parse_cue_file(cue_file_path).unwrap();

    for (index, track) in cue_sheet.tracks.iter().enumerate() {
        let cue_duration = &track.start_time;

        // Convert frames to milliseconds (1 CDDA frame = 1/75 second)
        let milliseconds = cue_duration.frames * 1000 / 75;

        // Convert minutes to hours and remaining minutes
        let hours = cue_duration.minutes / 60;
        let minutes = cue_duration.minutes % 60;

        // Format as "hh:mm:ss.mmm"
        let ffmpeg_start_time = format!("{:02}:{:02}:{:02}.{:03}", hours, minutes, cue_duration.seconds, milliseconds);

        // Calculate the end time based on the next track, if we have the last track, skip this param
        // let ffmpeg_end_time_param = format!("-to {:02}:{:02}:{:02}.{:03}", hours, minutes, cue_duration.seconds + 30, milliseconds);
        let ffmpeg_end_time = if index < cue_sheet.tracks.len() - 1 {
            let next_track = &cue_sheet.tracks[index + 1];
            let next_cue_duration = &next_track.start_time;
            let next_milliseconds = next_cue_duration.frames * 1000 / 75;
            let next_hours = next_cue_duration.minutes / 60;
            let next_minutes = next_cue_duration.minutes % 60;
            format!("-to \"{:02}:{:02}:{:02}.{:03}\"", next_hours, next_minutes, next_cue_duration.seconds, next_milliseconds)
        } else {
            "".to_string()
        };

        let output_file_name = format!("{}-{}-{}.mp3", track.number, track.artist, track.title);

        let command = format!("ffmpeg -i \"{}\" -acodec copy -ss \"{}\" {} \"{}\"", cue_sheet.file_name, ffmpeg_start_time, ffmpeg_end_time, output_file_name);

        println!("{}", command);
    }
}

#[derive(Debug)]
struct CueSheet {
    file_name: String,
    file_format: String,
    tracks: Vec<Track>,
}

#[derive(Debug)]
struct Track {
    number: u32,
    title: String,
    artist: String,
    start_time: CueDuration,
}

#[derive(Debug, Default)]
struct CueDuration {
    minutes: u32,
    seconds: u32,
    frames: u32,
}

impl CueDuration {
    fn new(minutes: u32, seconds: u32, frames: u32) -> Self {
        CueDuration { minutes, seconds, frames }
    }
}

fn parse_cue_file(file_path: &str) -> Option<CueSheet> {
    let mut file_name = String::new();
    let mut file_format = String::new();
    let mut tracks = Vec::new();
    let mut current_track = None;

    let file = File::open(file_path).unwrap();
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let line = line.unwrap();
        let tokens: Vec<&str> = line.split_whitespace().collect();

        if tokens.is_empty() {
            continue;
        }

        match tokens[0] {
            "FILE" => {
                file_name = tokens[1].replace("\"", "").to_string();
                file_format = tokens[2].replace("\"", "").to_string();
            }
            "TRACK" => {
                if let Some(track) = current_track.take() {
                    tracks.push(track);
                }

                let track_number = tokens[1].parse::<u32>().unwrap_or(0);
                current_track = Some(Track {
                    number: track_number,
                    title: String::new(),
                    start_time: CueDuration::new(0, 0, 0),
                    artist: String::new(),
                });
            }
            "TITLE" => {
                if let Some(ref mut track) = current_track {
                    track.title = tokens[1..].join(" ").replace("\"", "");
                }
            }
            "INDEX" => {
                if tokens[1] == "01" {
                    if let Some(ref mut track) = current_track {
                        let start_time = tokens[2].to_string();
                        let time_split = start_time.split(":").collect::<Vec<&str>>();
                        track.start_time = CueDuration::new(
                            time_split[0].parse::<u32>().unwrap(),
                            time_split[1].parse::<u32>().unwrap(),
                            time_split[2].parse::<u32>().unwrap(),
                        );
                    }
                }
            }
            "PERFORMER" => {
                if let Some(ref mut track) = current_track {
                    track.artist = tokens[1..].join(" ").replace("\"", "");
                }
            }
            _ => {}
        }
    }

    if let Some(track) = current_track {
        tracks.push(track);
    }

    Some(CueSheet { file_name, file_format, tracks })
}
