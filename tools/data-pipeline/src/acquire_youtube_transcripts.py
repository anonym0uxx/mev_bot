#!/usr/bin/env python
"""
acquire_youtube_transcripts.py - Fetch YouTube video transcripts for
memecoin trading content (Megga, Setuhh, and other elite creators).

Uses direct HTML fetch + immediate caption track fetch to get fresh tokens.
Bypasses the need for YouTube API keys.

Outputs raw_social_event_v1 format JSONL.
"""

import json
import urllib.request
import urllib.parse
import re
import time
import os
import argparse
import hashlib
from datetime import datetime
from concurrent.futures import ThreadPoolExecutor, as_completed

# YouTube transcript fetch via timedtext API
# The approach: fetch watch page HTML, extract caption track baseUrl,
# then immediately fetch the caption track with the fresh token

USER_AGENT = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"

# Elite memecoin YouTube channels/search queries
YOUTUBE_TARGETS = {
    "megga": {
        "name": "Megga",
        "search_queries": [
            "megga memecoin trading solana",
            "megga faze crypto stream",
            "megga trading memecoins guide",
        ],
        "known_videos": [
            ("cfPTjd7x3ng", "How I Made $15,000 Trading Memecoins In ONE DAY (FULL WALKTHROUGH)"),
            ("OhAdYpe_RHg", "I Turned $200 into $2000 Trading Memecoins (Full Guide)"),
        ],
    },
    "setuhh": {
        "name": "Setuhh",
        "search_queries": [
            "setuhh memecoin scalping strategy",
            "setuhh trading solana guide",
        ],
        "known_videos": [
            ("P1WjCGzIE2A", "How I Turned $50 into $500,000 Trading Memecoins (Full Scalping Guide)"),
            ("-GwaA1K11kA", "I Tried Turning $50 Into $1000 Trading Memecoins (Realistic Results)"),
        ],
    },
    "gravysol": {
        "name": "gravy",
        "search_queries": [
            "gravy solana memecoin trading",
        ],
        "known_videos": [
            ("__kKnxxemQk", "I Traded Solana Memecoins With 0.1 Solana (Realistic Results)"),
        ],
    },
}


def fetch_url(url, timeout=30):
    """Fetch URL with proper headers."""
    req = urllib.request.Request(url, headers={
        "User-Agent": USER_AGENT,
        "Accept-Language": "en-US,en;q=0.9",
        "Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
    })
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return resp.read(), resp.headers, resp.status
    except Exception as e:
        return None, None, str(e)


def get_video_metadata(video_id):
    """Get video title, description, publish date from watch page HTML."""
    url = f"https://www.youtube.com/watch?v={video_id}"
    html, _, status = fetch_url(url)
    if html is None:
        return None
    
    html = html.decode('utf-8', errors='ignore')
    
    # Extract title
    title_match = re.search(r'<title>([^<]+)</title>', html)
    title = title_match.group(1).replace(' - YouTube', '') if title_match else ''
    
    # Extract description
    desc_match = re.search(r'"shortDescription":"([^"]+)"', html)
    if not desc_match:
        desc_match = re.search(r'"description":"([^"]+)"', html)
    description = desc_match.group(1) if desc_match else ''
    description = description.replace('\\n', '\n').replace('\\/', '/')
    
    # Extract publish date
    date_match = re.search(r'"uploadDate":"([^"]+)"', html)
    publish_date = date_match.group(1) if date_match else ''
    
    # Extract channel name
    channel_match = re.search(r'"author":"([^"]+)"', html)
    channel = channel_match.group(1) if channel_match else ''
    
    # Extract view count
    views_match = re.search(r'"viewCount":"(\d+)"', html)
    views = int(views_match.group(1)) if views_match else 0
    
    # Extract length
    length_match = re.search(r'"lengthSeconds":"(\d+)"', html)
    length_sec = int(length_match.group(1)) if length_match else 0
    
    return {
        'video_id': video_id,
        'title': title,
        'description': description,
        'publish_date': publish_date,
        'channel': channel,
        'views': views,
        'length_seconds': length_sec,
    }


def get_transcript(video_id, timeout=30):
    """Get transcript for a YouTube video by fetching fresh caption tokens."""
    url = f"https://www.youtube.com/watch?v={video_id}"
    html, _, status = fetch_url(url, timeout=timeout)
    if html is None:
        return None, "HTML fetch failed"
    
    html = html.decode('utf-8', errors='ignore')
    
    # Find captionTracks in ytInitialPlayerResponse
    cap_match = re.search(r'"captionTracks":\s*\[([^\]]+)\]', html)
    if not cap_match:
        return None, "No captionTracks found (video may have no captions)"
    
    # Extract baseUrl
    url_match = re.search(r'"baseUrl":\s*"(https?://[^"]+)"', cap_match.group(1))
    if not url_match:
        return None, "No baseUrl in captionTracks"
    
    caption_url = url_match.group(1).replace('\\u0026', '&').replace('\\/', '/')
    
    # Add json3 format
    if '?' in caption_url:
        caption_url += '&fmt=json3'
    else:
        caption_url += '?fmt=json3'
    
    # Immediately fetch captions with the fresh token
    cap_data, _, cap_status = fetch_url(caption_url, timeout=timeout)
    if cap_data is None:
        return None, f"Caption fetch failed: {cap_status}"
    
    if len(cap_data) == 0:
        return None, "Empty caption data"
    
    # Parse JSON3 format
    if cap_data.startswith(b'{"'):
        try:
            cap_json = json.loads(cap_data)
            texts = []
            for evt in cap_json.get('events', []):
                segs = evt.get('segs', [])
                text = ''.join(s.get('utf8', '') for s in segs).strip()
                if text and text != '\n':
                    texts.append(text)
            return ' '.join(texts), None
        except json.JSONDecodeError as e:
            return None, f"JSON parse error: {e}"
    
    # Try XML format
    if b'<' in cap_data[:20]:
        import xml.etree.ElementTree as ET
        try:
            root = ET.fromstring(cap_data)
            texts = []
            for child in root.iter():
                if child.tag.endswith('text') or child.tag == 'text':
                    text = child.text or ''
                    text = re.sub(r'&amp;', '&', text)
                    text = re.sub(r'<[^>]+>', '', text)
                    text = text.strip()
                    if text:
                        texts.append(text)
            return ' '.join(texts), None
        except Exception as e:
            return None, f"XML parse error: {e}"
    
    return None, f"Unknown format: {cap_data[:50]}"


def search_youtube(query, limit=15):
    """Search YouTube and return video IDs with titles."""
    url = f"https://www.youtube.com/results?search_query={urllib.parse.quote(query)}"
    html, _, status = fetch_url(url, timeout=30)
    if html is None:
        return []
    
    html = html.decode('utf-8', errors='ignore')
    
    # Extract video IDs and titles
    video_ids = re.findall(r'"videoId":"([a-zA-Z0-9_-]{11})"', html)
    titles = re.findall(r'"title":{"runs":\[{"text":"([^"]+)"', html)
    
    # Also try simpler patterns
    if not titles:
        title_matches = re.findall(r'title.*?>([^<]{10,100})<', html[:50000])
        titles = [t.strip() for t in title_matches[:limit]]
    
    results = []
    seen = set()
    for i, vid in enumerate(video_ids):
        if vid in seen:
            continue
        seen.add(vid)
        title = titles[i] if i < len(titles) else ''
        results.append({'video_id': vid, 'title': title})
        if len(results) >= limit:
            break
    
    return results


def build_raw_event(video_id, metadata, transcript, run_uuid, git_sha):
    """Build a raw_social_event_v1 event from YouTube transcript."""
    publish_time_ms = 0
    if metadata.get('publish_date'):
        try:
            dt = datetime.fromisoformat(metadata['publish_date'].replace('Z', '+00:00'))
            publish_time_ms = int(dt.timestamp() * 1000)
        except:
            pass
    
    content = transcript
    if metadata.get('description'):
        content = f"{metadata['description']}\n\n--- TRANSCRIPT ---\n{transcript}"
    
    event = {
        'event_id': f"yt_{video_id}",
        'source_type': 'youtube',
        'source_name': metadata.get('channel', 'unknown'),
        'source_url': f"https://www.youtube.com/watch?v={video_id}",
        'content_text': content,
        'content_hash': hashlib.sha256(content.encode()).hexdigest()[:16],
        'publish_time_ms': publish_time_ms,
        'first_seen_ms': int(time.time() * 1000),
        'retrieval_time_ms': int(time.time() * 1000),
        'author_handle': metadata.get('channel', ''),
        'author_display_name': metadata.get('channel', ''),
        'platform': 'youtube',
        'engagement_metrics': {
            'views': metadata.get('views', 0),
            'length_seconds': metadata.get('length_seconds', 0),
        },
        'run_uuid': run_uuid,
        'git_sha': git_sha,
        'pipeline_version': 'narrative_gold_v1',
        'schema_version': 'raw_social_event_v1',
    }
    return event


def main():
    parser = argparse.ArgumentParser(description='Acquire YouTube transcripts for memecoin trading content')
    parser.add_argument('--creators', default='all', help='Comma-separated creator names (default: all)')
    parser.add_argument('--max-videos', type=int, default=10, help='Max videos per creator')
    parser.add_argument('--include-search', action='store_true', help='Include YouTube search results')
    parser.add_argument('--transcripts', action='store_true', help='Fetch transcripts (not just metadata)')
    parser.add_argument('--workers', type=int, default=3, help='Parallel workers')
    args = parser.parse_args()
    
    # Get git SHA
    git_sha = 'unknown'
    try:
        import subprocess
        r = subprocess.run(['git', 'rev-parse', '--short', 'HEAD'],
                          capture_output=True, text=True, cwd='D:/repos/mev_bot')
        git_sha = r.stdout.strip()[:7]
    except:
        pass
    
    run_uuid = f"yt_{int(time.time()):x}"
    
    # Select creators
    if args.creators == 'all':
        creators = list(YOUTUBE_TARGETS.keys())
    else:
        creators = [c.strip() for c in args.creators.split(',')]
    
    all_video_ids = []
    
    # Collect video IDs from known videos and search
    for creator_name in creators:
        if creator_name not in YOUTUBE_TARGETS:
            print(f"  Unknown creator: {creator_name}")
            continue
        
        target = YOUTUBE_TARGETS[creator_name]
        print(f"\n=== {target['name']} ===")
        
        # Known videos
        for vid_id, title in target.get('known_videos', []):
            all_video_ids.append((vid_id, creator_name, title))
            print(f"  Known: {title[:60]} ({vid_id})")
        
        # Search results
        if args.include_search:
            for query in target.get('search_queries', []):
                print(f"  Searching: {query}")
                results = search_youtube(query, limit=args.max_videos)
                for r in results:
                    if r['video_id'] not in [v[0] for v in all_video_ids]:
                        all_video_ids.append((r['video_id'], creator_name, r.get('title', '')))
                        print(f"    Found: {r.get('title', '')[:60]} ({r['video_id']})")
                time.sleep(2)
    
    print(f"\n=== Total videos: {len(all_video_ids)} ===")
    
    if not args.transcripts:
        print("\n--transcripts not set, exiting (use --transcripts to fetch)")
        return
    
    # Fetch transcripts
    output_dir = "output/narrative_gold_v1/raw/raw_social_event_v1"
    os.makedirs(output_dir, exist_ok=True)
    
    output_file = os.path.join(output_dir, f"youtube_transcripts_{run_uuid}.jsonl")
    
    def fetch_one(vid_info):
        vid_id, creator, title = vid_info
        # Get metadata
        meta = get_video_metadata(vid_id)
        if not meta:
            return None
        
        # Get transcript
        transcript, error = get_transcript(vid_id)
        if transcript is None:
            print(f"  [{vid_id}] No transcript: {error}")
            return None
        
        print(f"  [{vid_id}] Got transcript: {len(transcript)} chars")
        
        event = build_raw_event(vid_id, meta, transcript, run_uuid, git_sha)
        return event
    
    events = []
    with ThreadPoolExecutor(max_workers=args.workers) as executor:
        futures = {executor.submit(fetch_one, v): v for v in all_video_ids}
        for future in as_completed(futures):
            result = future.result()
            if result:
                events.append(result)
    
    # Write output
    with open(output_file, 'w', encoding='utf-8') as f:
        for event in events:
            f.write(json.dumps(event) + '\n')
    
    print(f"\n=== SUMMARY ===")
    print(f"  Total videos processed: {len(all_video_ids)}")
    print(f"  Transcripts acquired: {len(events)}")
    print(f"  Output: {output_file}")
    
    # Show transcript stats
    if events:
        total_chars = sum(len(e['content_text']) for e in events)
        print(f"  Total transcript chars: {total_chars}")
        for e in events[:5]:
            print(f"    {e['source_name']}: {e['engagement_metrics'].get('views', 0)} views, {len(e['content_text'])} chars")


if __name__ == '__main__':
    main()
