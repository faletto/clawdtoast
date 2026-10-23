#!/usr/bin/env python3
"""Extract the supplied Ccryu clips; needs ffmpeg, ffprobe and Pillow.

Run from any directory. Sources live in assets/hololive/source; generated
RGBA strips and their exact presentation durations are bundled by the app.
"""

from collections import deque
from decimal import Decimal
import json
from pathlib import Path
import subprocess

from PIL import Image


ROOT = Path(__file__).resolve().parents[1] / "assets" / "hololive"
SIDE = 88


def transparent_frame(image):
    """Recover the original 4x pixel grid and remove the exterior white matte.

    Flood only the near-white background connected to the border so white
    clothes, eyes and highlights inside the character remain opaque.
    """
    image = image.resize((image.width // 4, image.height // 4), Image.Resampling.NEAREST)
    width, height = image.size
    pixels = list(image.getdata())
    background = bytearray(width * height)
    pending = deque()
    for y in range(height):
        pending.extend((y * width, y * width + width - 1))
    pending.extend(range(width))
    pending.extend(range((height - 1) * width, height * width))
    while pending:
        i = pending.popleft()
        if background[i]:
            continue
        rgb = pixels[i]
        if min(rgb) < 238 or max(rgb) - min(rgb) > 22:
            continue
        background[i] = 1
        x, y = i % width, i // width
        if x:
            pending.append(i - 1)
        if x + 1 < width:
            pending.append(i + 1)
        if y:
            pending.append(i - width)
        if y + 1 < height:
            pending.append(i + width)
    result = Image.new("RGBA", image.size)
    result.putdata([(*rgb, 255) if not background[i] else (0, 0, 0, 0)
                    for i, rgb in enumerate(pixels)])
    return result


def extract(source):
    probe = json.loads(subprocess.check_output([
        "ffprobe", "-v", "error", "-select_streams", "v:0",
        "-show_entries", "stream=width,height,duration:frame=best_effort_timestamp_time",
        "-of", "json", str(source),
    ]))
    stream = probe["streams"][0]
    width, height = stream["width"], stream["height"]
    timestamps = [Decimal(frame["best_effort_timestamp_time"]) for frame in probe["frames"]]
    end = timestamps[0] + Decimal(stream["duration"])
    durations = [int((b - a) * 1000) for a, b in
                 zip(timestamps, timestamps[1:] + [end])]
    if not durations or any(ms <= 0 for ms in durations):
        raise ValueError(f"Invalid presentation times: {source}")
    raw = subprocess.check_output([
        "ffmpeg", "-v", "error", "-i", str(source), "-map", "0:v:0",
        "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "rgb24", "-",
    ])
    stride = width * height * 3
    if len(raw) != stride * len(durations):
        raise ValueError(f"Frame count mismatch: {source}")
    frames = [transparent_frame(Image.frombytes("RGB", (width, height), raw[i:i + stride]))
              for i in range(0, len(raw), stride)]
    boxes = [frame.getbbox() for frame in frames]
    if any(box is None for box in boxes):
        raise ValueError(f"Empty character frame: {source}")
    # One crop for the entire loop keeps the character anchored as it moves.
    box = (min(b[0] for b in boxes), min(b[1] for b in boxes),
           max(b[2] for b in boxes), max(b[3] for b in boxes))
    crop_width, crop_height = box[2] - box[0], box[3] - box[1]
    ratio = (SIDE - 4) / max(crop_width, crop_height)
    size = (max(1, round(crop_width * ratio)), max(1, round(crop_height * ratio)))
    sheet = Image.new("RGBA", (SIDE * len(frames), SIDE))
    for i, frame in enumerate(frames):
        art = frame.crop(box).resize(size, Image.Resampling.NEAREST)
        sheet.paste(art, (i * SIDE + (SIDE - size[0]) // 2, (SIDE - size[1]) // 2))
    # One palette across the whole loop avoids frame-to-frame palette changes.
    # No dithering: keep hard pixel edges while reducing MP4 color noise and binary size.
    sheet = sheet.quantize(colors=256, method=Image.Quantize.FASTOCTREE,
                           dither=Image.Dither.NONE).convert("RGBA")
    sheet.save(ROOT / f"{source.stem}.png", optimize=True)
    return {"name": source.stem, "frame_durations_ms": durations}


def main():
    sources = sorted((ROOT / "source").glob("*.mp4"))
    if len(sources) != 15:
        raise ValueError("Expected the 15 supplied Hololive clips in assets/hololive/source")
    metadata = [extract(source) for source in sources]
    (ROOT / "timing.json").write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")
    for entry in metadata:
        durations = entry["frame_durations_ms"]
        print(f"{entry['name']}: {len(durations)} frames, {sum(durations)} ms")


if __name__ == "__main__":
    main()
