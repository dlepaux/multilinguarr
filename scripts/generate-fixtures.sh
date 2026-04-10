#!/usr/bin/env bash
set -euo pipefail

# Generate e2e test fixtures — requires ffmpeg.
# 6-minute synthetic video + audio clears Radarr's 5-min sample threshold.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
MEDIA_DIR="$SCRIPT_DIR/../tests/media"

mkdir -p "$MEDIA_DIR"
cd "$MEDIA_DIR"

echo "Generating 6-minute synthetic source..."
ffmpeg -f lavfi -i testsrc2=duration=360:size=320x240:rate=24 \
  -f lavfi -i sine=frequency=440:duration=360 \
  -c:v libx264 -preset ultrafast -crf 51 \
  -c:a aac -b:a 32k \
  source.mkv -y -loglevel error

echo "Generating multi-language fixture (eng/fre/spa)..."
ffmpeg -i source.mkv \
  -filter_complex "[0:a]asplit=3[a1][a2][a3]" \
  -map 0:v -map "[a1]" -map "[a2]" -map "[a3]" \
  -c:v copy \
  -c:a aac -b:a 32k \
  -metadata:s:a:0 language=eng -metadata:s:a:0 title="English" \
  -metadata:s:a:1 language=fre -metadata:s:a:1 title="French" \
  -metadata:s:a:2 language=spa -metadata:s:a:2 title="Spanish" \
  test-movie-multi.mkv -y -loglevel error

echo "Generating English-only fixture..."
ffmpeg -i source.mkv \
  -map 0:v -map 0:a \
  -c:v copy -c:a aac -b:a 32k \
  -metadata:s:a:0 language=eng -metadata:s:a:0 title="English" \
  test-movie-en.mkv -y -loglevel error

echo "Generating French-only fixture..."
ffmpeg -i source.mkv \
  -map 0:v -map 0:a \
  -c:v copy -c:a aac -b:a 32k \
  -metadata:s:a:0 language=fre -metadata:s:a:0 title="French" \
  test-movie-fr.mkv -y -loglevel error

cp test-movie-multi.mkv test-episode-multi.mkv
cp test-movie-en.mkv test-episode-en.mkv
cp test-movie-fr.mkv test-episode-fr.mkv

echo "Done:"
ls -lh test-*.mkv
