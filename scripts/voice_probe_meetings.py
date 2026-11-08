"""Mix LibriSpeech dev-clean speakers into 2-3 speaker "meetings" for the
voice probes in src-tauri/src/meeting/voice_identity/probe.rs.

Each speaker appears in several meetings, never with the same utterance twice.
Two conditions share one design: `clean` keeps the read speech as recorded;
`room` passes every meeting through one shared channel (a reverberant laptop
room, a narrowband call, or a headset) with background noise, the way all the
voices in one real meeting share one microphone or one codec.

uv run --with numpy --with soundfile scripts/voice_probe_meetings.py \
    <LibriSpeech dir> <out dir>
"""

import json
import random
import sys
from pathlib import Path

import numpy as np
import soundfile as sf

RATE = 16_000
MEETINGS = 80
SEED = 20261107
MAX_TURN_S = 20.0


def speakers_txt(root: Path) -> dict[str, str]:
    genders = {}
    for line in (root / "SPEAKERS.TXT").read_text().splitlines():
        if line.startswith(";"):
            continue
        parts = [part.strip() for part in line.split("|")]
        if len(parts) >= 3 and parts[2] == "dev-clean":
            genders[parts[0]] = parts[1]
    return genders


def inventory(root: Path) -> dict[str, dict[str, list[Path]]]:
    speakers: dict[str, dict[str, list[Path]]] = {}
    for speaker in sorted((root / "dev-clean").iterdir()):
        chapters = {}
        for chapter in sorted(speaker.iterdir()):
            chapters[chapter.name] = sorted(chapter.glob("*.flac"))
        speakers[speaker.name] = chapters
    return speakers


def level(samples: np.ndarray, dbfs: float) -> np.ndarray:
    rms = float(np.sqrt(np.mean(samples**2))) or 1e-9
    return samples * (10 ** (dbfs / 20) / rms)


def fade(samples: np.ndarray) -> np.ndarray:
    ramp = min(len(samples) // 2, int(0.02 * RATE))
    window = np.ones(len(samples))
    window[:ramp] = np.linspace(0, 1, ramp)
    window[-ramp:] = np.linspace(1, 0, ramp)
    return samples * window


def spectral_mask(samples: np.ndarray, low_hz: float, high_hz: float) -> np.ndarray:
    spectrum = np.fft.rfft(samples)
    freqs = np.fft.rfftfreq(len(samples), 1 / RATE)
    mask = np.ones_like(freqs)
    if low_hz > 0:
        mask *= 1 / (1 + (low_hz / np.maximum(freqs, 1e-3)) ** 8)
    if high_hz < RATE / 2:
        mask *= 1 / (1 + (freqs / high_hz) ** 8)
    return np.fft.irfft(spectrum * mask, n=len(samples))


def room_impulse(rng: np.random.Generator, rt60_s: float) -> np.ndarray:
    length = int(rt60_s * RATE)
    t = np.arange(length) / RATE
    tail = rng.standard_normal(length) * np.exp(-6.9 * t / rt60_s) * 0.3
    tail[: int(0.003 * RATE)] = 0
    tail[0] = 1.0
    return tail / np.sqrt(np.sum(tail**2))


def convolve(samples: np.ndarray, impulse: np.ndarray) -> np.ndarray:
    size = len(samples) + len(impulse) - 1
    n = 1 << (size - 1).bit_length()
    out = np.fft.irfft(np.fft.rfft(samples, n) * np.fft.rfft(impulse, n), n)
    return out[: len(samples)]


def noise(rng: np.random.Generator, count: int, pink: bool) -> np.ndarray:
    white = rng.standard_normal(count)
    if not pink:
        return white
    spectrum = np.fft.rfft(white)
    freqs = np.fft.rfftfreq(count, 1 / RATE)
    spectrum /= np.sqrt(np.maximum(freqs, 20.0))
    return np.fft.irfft(spectrum, n=count)


def add_noise(rng, samples: np.ndarray, snr_db: float, pink: bool) -> np.ndarray:
    voiced = samples[np.abs(samples) > 1e-4]
    speech_rms = float(np.sqrt(np.mean(voiced**2))) if len(voiced) else 1e-3
    floor = noise(rng, len(samples), pink)
    floor *= speech_rms / (10 ** (snr_db / 20)) / float(np.sqrt(np.mean(floor**2)))
    return samples + floor


def channel(rng: np.random.Generator, mix: np.ndarray, preset: str) -> np.ndarray:
    if preset == "laptop_room":
        out = convolve(mix, room_impulse(rng, rt60_s=float(rng.uniform(0.3, 0.5))))
        out = spectral_mask(out, 120, 7000)
        return add_noise(rng, out, float(rng.uniform(15, 22)), pink=True)
    if preset == "narrowband_call":
        out = spectral_mask(mix, 300, 3400)
        return add_noise(rng, out, float(rng.uniform(25, 32)), pink=False)
    if preset == "headset":
        out = spectral_mask(mix, 80, 8000)
        return add_noise(rng, out, float(rng.uniform(30, 38)), pink=False)
    raise ValueError(preset)


def main() -> None:
    root = Path(sys.argv[1])
    out = Path(sys.argv[2])
    rng_py = random.Random(SEED)
    genders = speakers_txt(root)
    speakers = inventory(root)
    unused = {
        speaker: {chapter: list(paths) for chapter, paths in chapters.items()}
        for speaker, chapters in speakers.items()
    }
    for chapters in unused.values():
        for paths in chapters.values():
            rng_py.shuffle(paths)
    counts = {speaker: 0 for speaker in speakers}
    last_chapter: dict[str, str] = {}

    design = []
    for index in range(MEETINGS):
        size = rng_py.choice([2, 3])
        order = sorted(speakers, key=lambda speaker: (counts[speaker], rng_py.random()))
        chosen = order[:size]
        turns_by_speaker = {}
        for speaker in chosen:
            counts[speaker] += 1
            chapters = unused[speaker]
            ranked = sorted(
                (chapter for chapter in chapters if chapters[chapter]),
                key=lambda chapter: (
                    chapter == last_chapter.get(speaker),
                    -len(chapters[chapter]),
                ),
            )
            target = rng_py.uniform(20.0, 50.0)
            picked = []
            total = 0.0
            for chapter in ranked:
                while chapters[chapter] and total < target:
                    path = chapters[chapter].pop()
                    duration = sf.info(path).duration
                    take = min(duration, MAX_TURN_S)
                    start = (
                        rng_py.uniform(0, duration - take) if duration > take else 0.0
                    )
                    picked.append((str(path), start, take, chapter))
                    total += take
                if total >= target:
                    last_chapter[speaker] = chapter
                    break
            turns_by_speaker[speaker] = picked
        sequence = []
        last = None
        while any(turns_by_speaker.values()):
            options = [
                s for s, turns in turns_by_speaker.items() if turns and s != last
            ]
            if not options:
                options = [s for s, turns in turns_by_speaker.items() if turns]
            speaker = max(
                options,
                key=lambda s: (sum(t[2] for t in turns_by_speaker[s]), rng_py.random()),
            )
            sequence.append((speaker, *turns_by_speaker[speaker].pop()))
            last = speaker
        design.append(
            {
                "id": f"m{index:03d}",
                "speakers": chosen,
                "turns": sequence,
                "levels": {speaker: rng_py.uniform(-26, -20) for speaker in chosen},
                "gaps": [rng_py.uniform(0.25, 1.0) for _ in sequence],
                "preset": rng_py.choice(["laptop_room", "narrowband_call", "headset"]),
            }
        )

    for condition in ("clean", "room"):
        directory = out / condition
        directory.mkdir(parents=True, exist_ok=True)
        truth = {"genders": genders, "meetings": []}
        for meeting in design:
            rng = np.random.default_rng(SEED + int(meeting["id"][1:]))
            pieces = [np.zeros(RATE)]
            cursor = 1.0
            turns = []
            for (speaker, path, start, take, chapter), gap in zip(
                meeting["turns"], meeting["gaps"]
            ):
                audio, rate = sf.read(path, dtype="float64")
                assert rate == RATE
                clip = audio[int(start * RATE) : int(start * RATE) + int(take * RATE)]
                clip = fade(level(clip, meeting["levels"][speaker]))
                pieces.append(clip)
                end = cursor + len(clip) / RATE
                turns.append(
                    {
                        "speaker": speaker,
                        "chapter": chapter,
                        "start_s": cursor,
                        "end_s": end,
                    }
                )
                pieces.append(np.zeros(int(gap * RATE)))
                cursor = end + int(gap * RATE) / RATE
            pieces.append(np.zeros(RATE))
            mix = np.concatenate(pieces)
            if condition == "room":
                mix = channel(rng, mix, meeting["preset"])
            peak = float(np.max(np.abs(mix)))
            if peak > 0.99:
                mix *= 0.99 / peak
            sf.write(directory / f"{meeting['id']}.wav", mix, RATE, subtype="PCM_16")
            truth["meetings"].append(
                {
                    "id": meeting["id"],
                    "speakers": meeting["speakers"],
                    "preset": meeting["preset"] if condition == "room" else "clean",
                    "turns": turns,
                }
            )
        (directory / "truth.json").write_text(json.dumps(truth, indent=1))
        print(condition, len(truth["meetings"]), "meetings")
    appearances = {speaker: count for speaker, count in counts.items()}
    print("appearances per speaker:", sorted(appearances.values()))


if __name__ == "__main__":
    main()
