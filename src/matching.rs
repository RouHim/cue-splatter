use std::path::PathBuf;

pub(crate) fn find_best_hamming_match(
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

pub(crate) fn hamming_distance(x: &[u8], y: &[u8]) -> usize {
    x.iter().zip(y.iter()).filter(|(a, b)| a != b).count()
}

pub(crate) fn find_best_levenshtein_match(
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
pub(crate) fn size_of_longest(a: &str, b: &str) -> usize {
    if a.len() > b.len() {
        a.len()
    } else {
        b.len()
    }
}

// Returns the length of the shortest string
pub(crate) fn size_of_shortest(a: &str, b: &str) -> usize {
    if a.len() < b.len() {
        a.len()
    } else {
        b.len()
    }
}
