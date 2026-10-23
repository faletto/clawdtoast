# Hololive pixel art

Artwork credit: **Ccryu**, as identified in the filenames of the 15 supplied clips.
The original filenames are recorded in `source/originals.json`; the MP4s are kept
under short filenames in `source/` so the bundled assets can be regenerated.

Run `python tools/gen_hololive.py` from the repository root with Pillow, ffmpeg
and ffprobe installed. These tools are needed only to regenerate the assets.

The converter recovers the clips' 4x pixel grid with nearest-neighbor sampling,
removes the exterior near-white matte, and uses one crop across every frame of
each animation. It fits the art proportionally inside an 88x88 transparent frame
with padding, then packs all frames into an 8-bit RGBA PNG strip. Each strip uses
one shared 256-color palette across its entire loop to reduce MP4 color noise and
keep the binary within its size budget. No dithering, interpolation or antialiasing
is applied. Compression artifacts already present in the MP4s can remain in the
extracted pixels.

`timing.json` contains the presentation duration of every frame. Most clips use
120 ms frames (8.33 fps); Fauna and Lamy use 110 ms (9.09 fps). Ame and Kiara have
variable frame durations. Playback preserves those durations and the full loop.

Select `presets/hololive.toml` to choose a random character for each popup.
The choice stays fixed through redraws, hover and silent attention updates.
