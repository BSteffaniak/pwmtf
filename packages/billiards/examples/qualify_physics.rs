use std::{env, hint::black_box, process::ExitCode, time::Instant};

use pwmtf_billiards::{TableGeometry, qualification_corpus, qualification_profile, simulate_shot};

fn main() -> ExitCode {
    let iterations = env::args()
        .nth(1)
        .map(|value| value.parse::<u32>())
        .transpose()
        .unwrap_or_else(|error| {
            eprintln!("invalid iteration count: {error}");
            Some(0)
        })
        .unwrap_or(100);
    if iterations == 0 {
        eprintln!("iteration count must be positive");
        return ExitCode::FAILURE;
    }

    let geometry = TableGeometry::standard();
    let profile = qualification_profile();
    let corpus = qualification_corpus();
    let started = Instant::now();
    let mut checksum = 0_u64;
    for _ in 0..iterations {
        for fixture in &corpus {
            let result =
                match simulate_shot(geometry, profile, fixture.state.clone(), fixture.command) {
                    Ok(result) => result,
                    Err(error) => {
                        eprintln!("fixture {} failed: {error}", fixture.name);
                        return ExitCode::FAILURE;
                    }
                };
            checksum = checksum.rotate_left(7).wrapping_add(
                result
                    .state
                    .to_bytes()
                    .into_iter()
                    .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
                        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
                    }),
            );
            black_box(&result);
        }
    }
    let elapsed = started.elapsed();
    let shots = u64::from(iterations) * u64::try_from(corpus.len()).unwrap_or(u64::MAX);
    let shots_per_second = shots as f64 / elapsed.as_secs_f64();
    println!(
        "shots={shots} elapsed_ms={} shots_per_second={shots_per_second:.2} checksum={checksum:016x}",
        elapsed.as_millis()
    );
    ExitCode::SUCCESS
}
