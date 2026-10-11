# LinuxKMS 実機 PoC・検証手順

Home Display の本番構成（Debian minimal / Raspberry Pi OS Lite 64-bit で X11/Wayland なしに
DRM/KMS へ直接描画）の検証手順と、現時点での検証状況をまとめます。

> **重要:** このリポジトリの開発は macOS 上で行っており、LinuxKMS・DRM/KMS・GPU ドライバー・
> libinput・systemd ユニットの動作は **まだ実機で一度も確認していません**。
> macOS でのビルド・テスト成功は Linux 実機での動作を保証しません。
> Slint の LinuxKMS バックエンドは experimental 扱いのため、以下の PoC を独立した検証項目とします。

## 1. 検証状況

| 項目 | 状況 |
|---|---|
| `--features kms` の Rust コードがコンパイルできる (macOS ホストで `cargo check`) | ✅ 確認済み |
| aarch64-linux へのクロス `cargo check` | ❌ 未実施（C クロスコンパイラ / sysroot がないため `aws-lc-sys`・`fontconfig-sys` のビルドスクリプトで停止） |
| GitHub Actions のネイティブ amd64 / arm64 ビルドと `.deb` 生成 | ⏳ ワークフローは作成済み・**未実行** |
| LinuxKMS (femtovg / OpenGL ES) で全画面表示 | ⏳ 未検証 |
| LinuxKMS (software renderer) で全画面表示 | ⏳ 未検証 |
| root 以外 (video/render/input グループ) で DRM master を取得できる | ⏳ 未検証 |
| libinput によるキー入力・タッチ | ⏳ 未検証 |
| systemd ユニットの hardening 下での起動 | ⏳ 未検証 |
| 72 時間以上の連続稼働でメモリが増え続けない | ⏳ 未検証 |
| 1280×720 出力 + `SLINT_SCALE_FACTOR=0.6667` | ⏳ 未検証 |

## 2. 必要なもの

### カーネル / ファームウェア
- KMS ドライバーが有効であること（`ls /dev/dri/` に `card*` と `renderD*` がある）。
  - Raspberry Pi: `/boot/firmware/config.txt` に `dtoverlay=vc4-kms-v3d`（Pi 4/5 の既定）。
- 1 つのコネクターにモニターが接続されていること。

### 実行時パッケージ（`.deb` の Depends で自動解決）
`libinput10`, `libudev1`, `libxkbcommon0`, `libgbm1`, `libegl1`, `libgles2`, `libdrm2`,
`libfontconfig1`, `libfreetype6`, `fonts-noto-cjk`, `ca-certificates`
（GPU 描画には Mesa の DRI ドライバー `libgl1-mesa-dri` / `libegl-mesa0` が必要です）

### ビルド時パッケージ（ネイティブビルドの場合）
```sh
sudo apt install build-essential pkg-config cmake clang \
  libinput-dev libudev-dev libxkbcommon-dev libgbm-dev libdrm-dev \
  libegl-dev libgles-dev libfontconfig-dev libfreetype-dev
cargo build --release --no-default-features --features kms
```

## 3. 権限モデル（root で実行しない）

`kms` フィーチャーは Slint の `backend-linuxkms-noseat` を使い、デバイスを直接 open します。

| デバイス | 必要な権限 |
|---|---|
| `/dev/dri/card*` (KMS) | `video` グループ |
| `/dev/dri/renderD*` (GPU) | `render` グループ |
| `/dev/input/event*` (libinput) | `input` グループ |

DRM master は「他にマスターを持つプロセスがない状態で最初に open したプロセス」に与えられます。
X11 / Wayland compositor / 他の KMS アプリが動いていないこと、`getty@tty1` と競合しないこと
（ユニットに `Conflicts=getty@tty1.service` を設定済み）を確認してください。

うまくいかない場合の代替: **seatd + libseat**
```sh
sudo apt install seatd libseat-dev
sudo usermod -aG _seatd home-display   # ディストリビューションにより group 名が異なる
cargo build --release --no-default-features --features kms-libseat
```

## 4. PoC 手順（実機で 1 回ずつ記録する）

1. **デバイス確認**
   ```sh
   ls -l /dev/dri /dev/input
   id home-display
   ```
2. **出力とモードの一覧**（コネクター名・モード番号を確認）
   ```sh
   sudo -u home-display SLINT_DRM_OUTPUT=list /usr/bin/home-display --fixture default
   sudo -u home-display SLINT_DRM_MODE=list   /usr/bin/home-display --fixture default
   ```
3. **GPU レンダラー**（フィクスチャで外部通信なし）
   ```sh
   sudo systemctl stop getty@tty1
   sudo -u home-display SLINT_BACKEND=linuxkms-femtovg /usr/bin/home-display --fixture default
   ```
   - 期待: 1920×1080 で Overview が表示され、20 秒後に Radar へ切り替わる。
   - 記録: 起動時間、CPU 使用率 (`top`)、RSS、描画の乱れの有無。
4. **CPU レンダラー**（GPU ドライバーに問題がある場合のフォールバック）
   ```sh
   sudo -u home-display SLINT_BACKEND=linuxkms-software /usr/bin/home-display --fixture default
   ```
5. **入力**: キーボードを接続し `1` / `2` / `S` / `Space` / `Esc` が効くか。タッチパネルなら右上長押しで設定画面。
6. **プロファイル**: `--profile low` と `--profile full` で Radar のコマ数・アニメーション・メモリを比較。
7. **720p 出力**: `SLINT_DRM_MODE=<720p の番号> SLINT_SCALE_FACTOR=0.6667` でレイアウトが収まるか。
8. **回転設置**: `SLINT_KMS_ROTATION=90` など。
9. **systemd 経由**:
   ```sh
   sudo systemctl start home-display
   journalctl -u home-display -f
   systemd-analyze security home-display
   ```
   hardening で起動しない場合は `journalctl` の EPERM / SIGSYS を確認し、
   `systemctl edit home-display` で該当設定を緩めて原因を特定する。
10. **ライブデータ**: `/etc/home-display/home-display.env` に iCal URL を設定し再起動。

## 5. 長時間稼働テスト（72 時間以上）

```sh
# 10 分ごとに RSS とスレッド数を記録
while sleep 600; do
  date +%FT%T; ps -o rss=,nlwp=,%cpu= -C home-display
done | tee ~/home-display-soak.log
```
確認項目:
- RSS が単調増加しない（Radar フレーム差し替え・画面切替ごとの増加がない）
- `journalctl -u home-display --since -1d | grep -c WARN` が想定内（ネットワーク断の再試行は最大 30 分間隔）
- ネットワークを抜く → 各パネルが「古いデータ」表示になり、復帰後に自動回復する
- 日付をまたいで予定・祝日・曜日が切り替わる

## 6. 想定されるリスクと対処

| リスク | 対処 |
|---|---|
| LinuxKMS バックエンドが experimental | Slint を `=1.18.1` に固定。更新時は本手順を再実施 |
| GPU ドライバーで femtovg が動かない | `SLINT_BACKEND=linuxkms-software` に切り替え（Low プロファイル推奨） |
| DRM master を取得できない | getty / 他の compositor の停止、または `kms-libseat` + seatd |
| `MemoryDenyWriteExecute` 等の hardening で Mesa が落ちる | 既定で無効化済み。追加で緩める必要があれば drop-in で調整 |
| フォントが無い／豆腐になる | `fonts-noto-cjk` を Depends に指定済み。`display.font_family` で変更可 |
| 1920×1080 以外のパネル | `SLINT_SCALE_FACTOR`（または `display.scale_factor`）で論理 1920×1080 に合わせる |
