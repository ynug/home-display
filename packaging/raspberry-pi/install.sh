#!/usr/bin/env bash
# Home Display — installer / updater for Raspberry Pi OS Lite (64-bit) and Debian.
#
# Installs the .deb built by GitHub Actions, then (interactively) sets the
# secret iCal URL, the location, and the few OS settings the display needs.
# Re-running it updates to the newest release and keeps all settings.
#
#   sudo ./install.sh --repo OWNER/REPO                 # latest GitHub release
#   sudo ./install.sh --repo OWNER/REPO --version v0.0.3
#   sudo ./install.sh --deb ./home-display_0.0.3_arm64.deb
#   sudo ./install.sh --deb ./home-display-linux-arm64.zip   # Actions artifact
#
# Secrets: the iCal URL is read from the terminal without echo, written only to
# /etc/home-display/home-display.env (0600 root), and never printed or passed
# as a command-line argument. For a private repository use an authenticated
# `gh` CLI, or GITHUB_TOKEN (sudo --preserve-env=GITHUB_TOKEN ./install.sh ...).

set -euo pipefail

PKG=home-display
ETC_DIR=/etc/home-display
ENV_FILE=$ETC_DIR/home-display.env
CONFIG_FILE=$ETC_DIR/config.toml
STATE_DIR=/var/lib/home-display
CACHE_DIR=/var/cache/home-display
OVERRIDES=$STATE_DIR/ui-overrides.toml

REPO=${HOME_DISPLAY_REPO:-}
VERSION=""
DEB_SRC=""
ASSUME_YES=0
START=1
NEED_REBOOT=0

usage() {
    cat <<'EOF'
使い方: sudo ./install.sh [オプション]

  --repo OWNER/REPO   GitHub リポジトリ（.deb を Releases から取得）
  --version vX.Y.Z    取得するリリースのタグ（省略時は最新）
  --deb PATH          手元の .deb、または Actions の成果物 .zip を使う（ダウンロードしない）
  --yes               質問せずに実行（iCal URL・地点・OS 設定は変更しない）
  --no-start          インストール後にサービスを起動しない
  -h, --help          このヘルプ

再実行すると新しいリリースに更新します。設定（config.toml・iCal URL・画面で保存した値）は保持されます。
アンインストール: sudo apt purge home-display
EOF
}

info() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m警告:\033[0m %s\n' "$*" >&2; }
die() {
    printf '\033[1;31mエラー:\033[0m %s\n' "$*" >&2
    exit 1
}

# Prompts read from the terminal so `curl ... | sudo bash` also works.
ask_yn() { # ask_yn "question" default(y|n)
    local q=$1 def=$2 ans hint="[y/N]"
    [ "$def" = y ] && hint="[Y/n]"
    if [ "$ASSUME_YES" = 1 ]; then
        return 1
    fi
    read -r -p "$q $hint " ans </dev/tty || ans=""
    ans=${ans:-$def}
    case $ans in [yY]*) return 0 ;; *) return 1 ;; esac
}

ask() { # ask "prompt" -> REPLY
    REPLY=""
    read -r -p "$1" REPLY </dev/tty || true
}

while [ $# -gt 0 ]; do
    case $1 in
    --repo) REPO=${2:?--repo に値が必要です}; shift 2 ;;
    --version) VERSION=${2:?--version に値が必要です}; shift 2 ;;
    --deb) DEB_SRC=${2:?--deb に値が必要です}; shift 2 ;;
    --yes | -y) ASSUME_YES=1; shift ;;
    --no-start) START=0; shift ;;
    -h | --help) usage; exit 0 ;;
    *) usage >&2; die "不明なオプション: $1" ;;
    esac
done

[ "$(id -u)" = 0 ] || die "root で実行してください（sudo ./install.sh ...）"
if [ "$ASSUME_YES" = 0 ] && ! { : </dev/tty; } 2>/dev/null; then
    die "端末がありません。対話なしで実行するには --yes を付けてください"
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# ───────────── 1. 事前確認 ─────────────

preflight() {
    ARCH=$(dpkg --print-architecture)
    case $ARCH in
    arm64 | amd64) ;;
    armhf | armel)
        die "32-bit OS（$ARCH）には対応していません。Raspberry Pi OS Lite (64-bit) を入れ直してください" ;;
    *) die "未対応のアーキテクチャです: $ARCH" ;;
    esac

    local codename="" pretty="" model=""
    if [ -r /etc/os-release ]; then
        # shellcheck disable=SC1091
        codename=$(. /etc/os-release && echo "${VERSION_CODENAME:-}")
        # shellcheck disable=SC1091
        pretty=$(. /etc/os-release && echo "${PRETTY_NAME:-}")
    fi
    [ -r /proc/device-tree/model ] && model=$(tr -d '\0' </proc/device-tree/model)
    info "機種: ${model:-不明} / OS: ${pretty:-不明} / $ARCH"
    case $codename in
    bookworm | trixie) ;;
    *) warn "Debian 12 (bookworm) / 13 (trixie) 系以外では未確認です（${codename:-不明}）" ;;
    esac

    if ! ls /dev/dri/card* >/dev/null 2>&1; then
        warn "/dev/dri/card* がありません。KMS ドライバーが無効の可能性があります"
        warn "（Raspberry Pi: /boot/firmware/config.txt に dtoverlay=vc4-kms-v3d）"
    fi
}

# ───────────── 2. .deb の取得 ─────────────

# Prints "name<TAB>browser_url<TAB>api_url<TAB>digest" for the asset matching $ARCH.
pick_asset() {
    python3 - "$1" "$ARCH" <<'PY'
import json, sys
release = json.load(open(sys.argv[1]))
suffix = "_%s.deb" % sys.argv[2]
for a in release.get("assets", []):
    if a["name"].endswith(suffix):
        print("\t".join([a["name"], a["browser_download_url"], a["url"], a.get("digest") or ""]))
        break
PY
}

download_release() {
    [ -n "$REPO" ] || die "--repo OWNER/REPO（または --deb）を指定してください"
    case $REPO in */*) ;; *) die "--repo は OWNER/REPO の形式で指定してください: $REPO" ;; esac

    if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
        info "gh で $REPO のリリース ${VERSION:-（最新）} を取得します"
        # shellcheck disable=SC2086
        gh release download ${VERSION:+"$VERSION"} -R "$REPO" -p "*_${ARCH}.deb" -D "$TMP" ||
            die "リリースからダウンロードできませんでした"
        DEB=$(ls "$TMP"/*_"$ARCH".deb | head -n 1)
        return
    fi

    command -v curl >/dev/null || apt-get install -y --no-install-recommends curl
    command -v python3 >/dev/null || apt-get install -y --no-install-recommends python3

    # The token goes into a 0600 header file, not onto curl's command line.
    local hdr=$TMP/headers
    (umask 077 && printf 'Accept: application/vnd.github+json\nX-GitHub-Api-Version: 2022-11-28\n' >"$hdr")
    if [ -n "${GITHUB_TOKEN:-}" ]; then
        printf 'Authorization: Bearer %s\n' "$GITHUB_TOKEN" >>"$hdr"
    fi

    local api="https://api.github.com/repos/$REPO/releases/latest"
    [ -n "$VERSION" ] && api="https://api.github.com/repos/$REPO/releases/tags/$VERSION"
    info "GitHub から $REPO のリリース ${VERSION:-（最新）} を確認しています"
    curl -fsSL -H @"$hdr" -o "$TMP/release.json" "$api" ||
        die "リリース情報を取得できません（リポジトリ名・タグ・公開設定を確認。非公開なら gh か GITHUB_TOKEN）"

    local line name url api_url digest
    line=$(pick_asset "$TMP/release.json")
    [ -n "$line" ] || die "リリースに *_${ARCH}.deb がありません"
    IFS=$'\t' read -r name url api_url digest <<<"$line"

    info "ダウンロード: $name"
    if [ -n "${GITHUB_TOKEN:-}" ]; then
        # Private repositories: assets are only reachable through the API URL.
        sed -i 's|^Accept: .*|Accept: application/octet-stream|' "$hdr"
        curl -fSL --progress-bar -H @"$hdr" -o "$TMP/$name" "$api_url"
    else
        curl -fSL --progress-bar -o "$TMP/$name" "$url"
    fi
    DEB=$TMP/$name

    if [ -n "$digest" ] && [ "${digest%%:*}" = sha256 ]; then
        echo "${digest#sha256:}  $DEB" | sha256sum -c --quiet - || die "SHA-256 が一致しません"
        info "SHA-256 を確認しました"
    fi
}

local_deb() {
    [ -f "$DEB_SRC" ] || die "ファイルがありません: $DEB_SRC"
    case $DEB_SRC in
    *.zip)
        command -v unzip >/dev/null || apt-get install -y --no-install-recommends unzip
        unzip -q -j -o "$DEB_SRC" '*.deb' -d "$TMP" || die "zip に .deb が含まれていません"
        DEB=$(ls "$TMP"/*.deb | head -n 1)
        ;;
    *.deb) cp "$DEB_SRC" "$TMP/" && DEB=$TMP/$(basename "$DEB_SRC") ;;
    *) die "--deb には .deb か .zip を指定してください" ;;
    esac
}

install_deb() {
    local deb_arch old new
    deb_arch=$(dpkg-deb -f "$DEB" Architecture)
    [ "$deb_arch" = "$ARCH" ] || die "パッケージは $deb_arch 用です（この機器は $ARCH）"
    old=$(dpkg-query -W -f='${Version}' "$PKG" 2>/dev/null || true)
    new=$(dpkg-deb -f "$DEB" Version)
    info "インストール: $PKG ${old:+$old → }$new"

    apt-get update -q
    # Keep a locally edited /etc/home-display/config.toml on upgrades.
    DEBIAN_FRONTEND=noninteractive apt-get install -y \
        -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold "$DEB"
    if [ -f "$CONFIG_FILE.dpkg-dist" ]; then
        warn "新しい既定の設定が $CONFIG_FILE.dpkg-dist にあります（今の config.toml はそのまま）"
    fi
}

# ───────────── 3. iCal URL（秘密） ─────────────

configure_ical() {
    [ -f "$ENV_FILE" ] || install -m 0600 -o root -g root /usr/share/home-display/home-display.env.example "$ENV_FILE"
    if grep -q '^[[:space:]]*HOME_DISPLAY_ICAL_URL=.' "$ENV_FILE"; then
        echo "カレンダー: iCal URL は設定済みです（内容は表示しません）"
        ask_yn "iCal URL を変更しますか？" n || return 0
    else
        echo "カレンダー: iCal URL は未設定です（未設定でも他の表示は動きます）"
        ask_yn "Google カレンダーの「iCal 形式の非公開アドレス」を設定しますか？" y || return 0
    fi

    local url="" ok='^https://[^[:space:]"'\''\\]+$'
    read -r -s -p "iCal URL を貼り付けて Enter（画面には表示されません）: " url </dev/tty || true
    echo
    if [ -z "$url" ]; then
        echo "変更しませんでした"
        return 0
    fi
    if ! [[ $url =~ $ok ]]; then
        warn "https:// で始まる、空白や引用符を含まない URL を入力してください。変更しませんでした"
        return 0
    fi

    local tmp
    tmp=$(mktemp "$ETC_DIR/.home-display.env.XXXXXX")
    chmod 0600 "$tmp"
    grep -v '^[[:space:]]*HOME_DISPLAY_ICAL_URL=' "$ENV_FILE" >"$tmp" || true
    printf 'HOME_DISPLAY_ICAL_URL=%s\n' "$url" >>"$tmp" # printf is a builtin: no argv exposure
    url=""
    chown root:root "$tmp"
    mv -f "$tmp" "$ENV_FILE"
    info "iCal URL を $ENV_FILE（0600 root）に保存しました"
}

# ───────────── 4. 地点 ─────────────

# "lat, lon" -> "lat, lon" normalised, or non-zero exit.
parse_coords() {
    awk -v s="$1" 'BEGIN {
        gsub(/[ \t]/, "", s)
        n = split(s, p, ",")
        num = "^[-+]?[0-9]+(\\.[0-9]+)?$"
        if (n != 2 || p[1] !~ num || p[2] !~ num) exit 1
        lat = p[1] + 0; lon = p[2] + 0
        if (lat < -90 || lat > 90 || lon < -180 || lon > 180) exit 1
        printf "%s, %s\n", p[1], p[2]
    }'
}

configure_location() {
    local cur_name="" cur_loc=""
    if [ -f "$OVERRIDES" ]; then
        cur_name=$(sed -n 's/^location_name[[:space:]]*=[[:space:]]*"\(.*\)"[[:space:]]*$/\1/p' "$OVERRIDES")
        cur_loc=$(sed -n 's/^location[[:space:]]*=[[:space:]]*"\(.*\)"[[:space:]]*$/\1/p' "$OVERRIDES")
    fi
    if [ -n "$cur_loc" ]; then
        echo "地点: ${cur_name:-（名前なし）} ($cur_loc)  ※画面で保存した値"
    else
        echo "地点: $CONFIG_FILE の [location] を使用中"
        sed -n '/^\[location\]/,/^\[/{/^\(name\|latitude\|longitude\)[[:space:]]*=/p}' "$CONFIG_FILE" | sed 's/^/    /'
    fi
    ask_yn "地点を設定しますか？" n || return 0

    echo "Google マップで地点を右クリックすると「35.6812, 139.7671」の形式で座標をコピーできます。"
    local name coords
    ask "地点名（例: 札幌）: "
    name=$REPLY
    while :; do
        ask "座標「緯度, 経度」（半角）: "
        coords=$(parse_coords "$REPLY") && break
        warn "「43.0621, 141.3544」の形式で、範囲内の値を入力してください"
    done

    # TOML basic string: escape backslash and double quote.
    name=${name//\\/\\\\}
    name=${name//\"/\\\"}

    install -d -m 0755 -o "$PKG" -g "$PKG" "$STATE_DIR"
    local tmp
    tmp=$(mktemp "$STATE_DIR/.ui-overrides.XXXXXX")
    if [ -f "$OVERRIDES" ]; then
        grep -Ev '^(location_name|location)[[:space:]]*=' "$OVERRIDES" >"$tmp" || true
    else
        echo "# Written by install.sh (same format as the settings screen's save)." >"$tmp"
    fi
    printf 'location_name = "%s"\nlocation = "%s"\n' "$name" "$coords" >>"$tmp"
    chown "$PKG:$PKG" "$tmp"
    chmod 0600 "$tmp"
    mv -f "$tmp" "$OVERRIDES"
    info "地点を $OVERRIDES に保存しました（config.toml より優先されます）"
}

# ───────────── 5. OS の設定 ─────────────

configure_os() {
    # A desktop session (X11/Wayland) owns the display; the app needs it free.
    if [ "$(systemctl get-default)" = graphical.target ]; then
        warn "デスクトップ環境が起動する設定です。Home Display は画面を直接使うため、デスクトップと同時には動きません"
        if ask_yn "起動時にデスクトップを使わない設定（multi-user.target）にしますか？（要再起動）" y; then
            systemctl set-default multi-user.target
            NEED_REBOOT=1
        else
            warn "デスクトップが動いている間は表示できません"
        fi
    fi

    local tz
    tz=$(timedatectl show -p Timezone --value 2>/dev/null || true)
    if [ -n "$tz" ] && [ "$tz" != Asia/Tokyo ]; then
        echo "システムのタイムゾーン: $tz（表示は config.toml の timezone を使います。ログの時刻に影響）"
        if ask_yn "システムのタイムゾーンを Asia/Tokyo にしますか？" y; then
            timedatectl set-timezone Asia/Tokyo
        fi
    fi
    if [ "$(timedatectl show -p NTP --value 2>/dev/null || true)" = no ]; then
        info "時刻の自動同期（NTP）を有効にします"
        timedatectl set-ntp true || warn "NTP を有効にできませんでした"
    fi
}

# ───────────── 6. 確認・起動 ─────────────

check_config() {
    info "設定ファイルを検証しています"
    if ! runuser -u "$PKG" -- env STATE_DIRECTORY="$STATE_DIR" CACHE_DIRECTORY="$CACHE_DIR" \
        /usr/bin/home-display --check-config --config "$CONFIG_FILE"; then
        warn "設定に問題があります: sudo nano $CONFIG_FILE"
    fi
}

start_service() {
    systemctl daemon-reload
    systemctl enable "$PKG" >/dev/null
    if [ "$NEED_REBOOT" = 1 ]; then
        if ask_yn "今すぐ再起動しますか？（起動後に自動で表示が始まります）" y; then
            info "再起動します"
            systemctl reboot
            exit 0
        fi
        echo "再起動後に自動で表示が始まります: sudo reboot"
        return 0
    fi
    [ "$START" = 1 ] || return 0
    info "サービスを（再）起動します"
    systemctl restart "$PKG"
    sleep 5
    if systemctl is-active --quiet "$PKG"; then
        info "home-display は動作中です"
    else
        warn "起動していません。ログを確認してください: journalctl -u $PKG -n 50"
    fi
}

preflight
if [ -n "$DEB_SRC" ]; then local_deb; else download_release; fi
install_deb
configure_ical
configure_location
configure_os
check_config
start_service

cat <<EOF

完了しました。
  ログ:          journalctl -u $PKG -f
  再起動:        sudo systemctl restart $PKG
  設定ファイル:  $CONFIG_FILE（編集後は再起動）
  iCal URL:      $ENV_FILE（0600、root のみ）
  画面の保存値:  $OVERRIDES
  更新:          sudo $0 ${REPO:+--repo $REPO}
  削除:          sudo apt purge $PKG
EOF
