# Raspberry Pi へのインストールと自動起動

GitHub Actions がビルドした `.deb` を、Raspberry Pi に入れて電源投入時に自動で表示させる手順です。
作業はすべて別の PC から SSH で行えます（Pi 側にマウス・キーボードは不要です）。

> **未検証:** この手順とインストールスクリプトは、まだ Raspberry Pi の実機で試していません。
> 初回は [linux-kms.md](linux-kms.md) の PoC 手順と合わせて確認してください。

## 必要なもの

| 項目 | 内容 |
|---|---|
| 本体 | Raspberry Pi 4（2GB 以上）/ 5 推奨。Pi 3B+・Zero 2 W は Low プロファイル向け |
| OS | **Raspberry Pi OS Lite (64-bit)**（Debian 12 bookworm / 13 trixie 系）。32-bit 版は非対応 |
| 画面 | HDMI モニター（1920×1080 推奨。他の解像度は「画面の調整」を参照） |
| ネットワーク | 有線 LAN または Wi-Fi（気象庁・カレンダーの取得に必要） |

デスクトップ付きの Raspberry Pi OS でも動きますが、デスクトップと同時には表示できません
（スクリプトが起動時にデスクトップを使わない設定に変えるか確認します）。

## 1. SD カードの準備（Raspberry Pi Imager）

1. Raspberry Pi Imager で「Raspberry Pi OS (other)」→ **Raspberry Pi OS Lite (64-bit)** を選ぶ
2. 「設定を編集する」で次を設定して書き込む
   - ホスト名（例: `home-display`）
   - ユーザー名とパスワード
   - Wi-Fi（有線なら不要）、ロケール: タイムゾーン `Asia/Tokyo`
   - 「サービス」タブで **SSH を有効化**（公開鍵認証を推奨）
3. SD カードを挿して起動し、PC から接続する

```sh
ssh <ユーザー名>@home-display.local
```

## 2. リリースの作成（GitHub 側、初回と更新時）

`.deb` は `v*` タグを push したときに GitHub Release に添付されます（[ci.yml](../.github/workflows/ci.yml)）。

タグは `Cargo.toml` の `version` に `v` を付けたものにします（例: `version = "0.0.3"` → `v0.0.3`）。
一致しないと CI がビルド前に失敗します。版を上げるときは先に `Cargo.toml` を書き換え、
`cargo check` で `Cargo.lock` も更新してからコミットします。

```sh
git tag v0.0.3
git push origin v0.0.3
```

Release には `home-display_0.0.3_arm64.deb`（Pi 用）と `home-display_0.0.3_amd64.deb`、`install.sh` が添付されます
（ファイル名の版はタグから `v` を除いたものと同じ）。
タグを付けずに試す場合は、Actions の実行結果から成果物 `home-display-linux-arm64`（zip）をダウンロードし、
`scp` で Pi に送って `--deb` で指定します（下記）。

## 3. インストール（Pi 上で）

[`packaging/raspberry-pi/install.sh`](../packaging/raspberry-pi/install.sh) を使います。

```sh
# Release に添付された install.sh を取得（OWNER/REPO は自分のリポジトリ）
curl -fsSLO https://github.com/OWNER/REPO/releases/latest/download/install.sh
less install.sh                       # 実行前に内容を確認
sudo bash install.sh --repo OWNER/REPO
```

スクリプトが行うこと:

1. 機種・OS・アーキテクチャ（arm64）・`/dev/dri` を確認
2. 最新の Release から `*_arm64.deb` をダウンロード（GitHub が公開している SHA-256 があれば照合）
3. `apt` でインストール（必要なライブラリ・日本語フォントも自動で入ります）
4. **iCal URL** を入力（画面に表示されません。`/etc/home-display/home-display.env` に 0600 で保存）
5. **地点** を入力（地点名と「緯度, 経度」。日本語の地点名も可）
6. デスクトップが起動する設定なら、デスクトップなしの起動に変更するか確認。タイムゾーン・時刻同期を確認
7. 設定ファイルを検証し、サービスを有効化・起動

インストール後は電源を入れるだけで表示が始まります（`home-display.service` が自動起動し、
異常終了しても 5 秒後に再起動します）。

### その他の指定方法

```sh
sudo bash install.sh --repo OWNER/REPO --version v0.0.3     # 版を指定
sudo bash install.sh --deb ./home-display-linux-arm64.zip    # Actions の成果物（zip のまま可）
sudo bash install.sh --deb ./home-display_0.0.3_arm64.deb    # .deb を直接
sudo bash install.sh --repo OWNER/REPO --yes                 # 質問なし（更新用。設定は変えない）
```

**非公開リポジトリの場合:** `gh auth login` 済みの `gh` があれば自動で使います。
または、トークンをコマンド履歴に残さないように読み込んで渡します。

```sh
read -rs GITHUB_TOKEN && export GITHUB_TOKEN    # 貼り付けて Enter（表示されません）
sudo --preserve-env=GITHUB_TOKEN bash install.sh --repo OWNER/REPO
```

`curl .../install.sh` も非公開リポジトリでは取得できないので、`scp` で送るか `gh release download -p install.sh` を使います。

## 4. 動作確認

```sh
systemctl status home-display
journalctl -u home-display -f          # ログ（iCal URL は出力されません）
```

## 5. 更新

同じスクリプトをもう一度実行します。設定（`config.toml`・iCal URL・画面で保存した値）は保持されます。

```sh
sudo bash install.sh --repo OWNER/REPO --yes
```

## 6. 設定の変更

| 変更したいもの | 方法 |
|---|---|
| 地点・iCal URL | `sudo bash install.sh --repo OWNER/REPO` を再実行して質問に答える |
| プロファイル・表示時間・縮尺など | `sudo nano /etc/home-display/config.toml` → `sudo systemctl restart home-display` |
| 画面で保存した値の確認・修正 | `/var/lib/home-display/ui-overrides.toml`（[README の「地点」](../README.md#地点)参照） |

設定画面で保存した値は `config.toml` より優先されます。

## 7. 画面の調整

`sudo systemctl edit home-display` で次を追加し、`sudo systemctl restart home-display` します。

```ini
[Service]
# 出力先・解像度（一覧は linux-kms.md の PoC 手順 2）
Environment=SLINT_DRM_OUTPUT=HDMI-A-1
# 1280x720 の画面に 1920x1080 のレイアウトを縮小表示
Environment=SLINT_SCALE_FACTOR=0.6667
# 縦置き
Environment=SLINT_KMS_ROTATION=90
# GPU 描画で問題がある場合（CPU 描画。Low プロファイル推奨）
Environment=SLINT_BACKEND=linuxkms-software
```

## 8. 任意の OS 設定（スクリプトでは変更しません）

| 目的 | 方法 |
|---|---|
| Wi-Fi の省電力で通信が途切れる | `sudo nmcli connection modify <接続名> 802-11-wireless.powersave 2` |
| 起動時のカーソル点滅・ログを隠す | `/boot/firmware/cmdline.txt` の末尾に ` vt.global_cursor_default=0 quiet` |
| SD カードの書き込みを減らす | ログを RAM に置く: `/etc/systemd/journald.conf` に `Storage=volatile` |

アプリ自身はログファイルを書かず、書き込みは祝日 CSV・地図タイルのキャッシュと画面の保存値だけです。

## 9. アンインストール

```sh
sudo apt remove home-display     # 設定は残す
sudo apt purge home-display      # 設定・iCal URL・キャッシュ・専用ユーザーも削除
```

デスクトップ起動に戻す場合: `sudo systemctl set-default graphical.target && sudo reboot`
