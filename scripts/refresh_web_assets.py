import urllib.request
import json
import re
import os

# Configuration
REPO = "yuhuotech/shanji-app"
HTML_FILES = ["web/index.html", "web/download.html"]

def get_latest_release():
    print(f"Fetching latest release from GitHub for {REPO}...")
    url = f"https://api.github.com/repos/{REPO}/releases/latest"
    req = urllib.request.Request(url, headers={'User-Agent': 'Mozilla/5.0'})
    with urllib.request.urlopen(req) as response:
        data = json.loads(response.read().decode())
        
    version = data['tag_name']
    assets = {}
    
    # Accurate matching based on your provided naming convention
    for asset in data['assets']:
        name = asset['name'].lower()
        dl_url = asset['browser_download_url']
        
        # macOS
        if 'macos' in name:
            if 'aarch64' in name or 'arm64' in name: assets['mac-arm'] = dl_url
            elif 'x86_64' in name or 'x64' in name: assets['mac-x64'] = dl_url
        # Windows
        elif 'windows' in name:
            if '.zip' in name: assets['win-x64'] = dl_url
            elif '.exe' in name and 'win-x64' not in assets: assets['win-x64'] = dl_url # fallback
        # Linux
        elif 'linux' in name:
            if '.deb' in name: assets['linux-deb'] = dl_url
            elif '.rpm' in name: assets['linux-rpm'] = dl_url
            elif '.tar.gz' in name: assets['linux-tar'] = dl_url
            elif 'appimage' in name: assets['linux-appimage'] = dl_url
        
    return version, assets

def update_files(version, assets):
    for file_path in HTML_FILES:
        if not os.path.exists(file_path): continue
            
        with open(file_path, 'r', encoding='utf-8') as f:
            content = f.read()
            
        # 1. Update Version Text
        v_pattern = r'<!--VERSION_START-->.*?<!--VERSION_END-->'
        if 'id="version-text"' in content:
            new_v_html = f'<!--VERSION_START-->最新版本: {version}<!--VERSION_END-->'
        else:
            new_v_html = f'<!--VERSION_START-->{version}<!--VERSION_END-->'
        content = re.sub(v_pattern, new_v_html, content)
        
        # 2. Update Assets JSON
        a_pattern = r'/\*ASSETS_START\*/.*?/\*ASSETS_END\*/'
        if "/*ASSETS_START*/" in content:
            assets_json = json.dumps(assets, indent=12)
            new_a_html = f'/*ASSETS_START*/\n        const releaseAssets = {assets_json};\n        /*ASSETS_END*/'
            content = re.sub(a_pattern, new_a_html, content, flags=re.DOTALL)
            
        with open(file_path, 'w', encoding='utf-8') as f:
            f.write(content)
        print(f"Successfully updated {file_path}")

if __name__ == "__main__":
    try:
        v, a = get_latest_release()
        update_files(v, a)
        print(f"\nWebsite is now synced to {v}")
    except Exception as e:
        print(f"Error: {e}")
