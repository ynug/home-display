# 室内センサー（I2C）

Raspberry Pi の I2C に接続した温湿度・CO2・気圧センサーを、本体（Rust）が直接読み取って表示する機能の仕様と、
センサードライバーを追加するときの方針をまとめます。

> **状況:** 実装済み（ドライバー・設定・読み取り・画面表示・サンプル値・`install-raspberry-pi.sh` での導入）。
> Raspberry Pi 4 実機とセンサー基板で、約 1 週間連続で使って問題ないことを確認しています。
> センサーを追加するときの手引きとしても更新していきます。

## 1. 概要

- `config.toml` に `[sensors]` を書いたときだけ動きます。書かなければ何もせず、画面も今と同じです。
- センサーごとに **型番（`model`）と I2C アドレス** を指定します。
  どの値をどのセンサーから表示するかも config で指定し、指定のない値は表示しません。
- 読み取りは Linux（`/dev/i2c-*`）のみ。macOS などでは fixture のサンプル値で表示を確認します。
- 外部プロセスや中間ファイルは使いません。センサーの追加 = Rust のドライバーを 1 つ追加、です。

### 対応センサー

| `model` | 型番 | 測定値 | I2C アドレス | 最短の更新間隔 |
|---|---|---|---|---|
| `sht35` | Sensirion SHT35（SHT3x 系） | 温度・湿度 | `0x44`（ADDR=L）/ `0x45`（ADDR=H） | 1 秒 |
| `scd41` | Sensirion SCD41 | CO2・温度・湿度 | `0x62`（固定） | **5 秒**（センサーの仕様） |
| `bmp581` | Bosch BMP581 | 気圧・温度 | `0x46`（SDO=L）/ `0x47`（SDO=H） | 1 秒 |

接続後のアドレスは `i2cdetect -y 1` で確認できます（`i2c-tools` パッケージ）。

### 本機の基板（使用が決まっている構成）

| チップ | 部品番号（LCSC） | I2C アドレス | 測るもの | アドレスの決め方 |
|---|---|---|---|---|
| SCD41 | C3659294 | `0x62` | CO2・温度・湿度 | 固定 |
| SHT35-DIS-B | C90161 | `0x44` | 温度・湿度（高精度） | ADDR = GND |
| BMP581 | C5362283 | `0x47` | 気圧・温度 | SDO = 3V3 |

この構成は `install-raspberry-pi.sh`（`--sensors`）が書き込む `[sensors]` と、サンプルデータ（fixture）の既定です
（コード上は `SensorsConfig::standard()`）。温度・湿度は自己発熱の少ない SHT35 を表示し、CO2 は BMP581 の気圧で補正します。

基板の回路・製造データ（JLCPCB 用）・注文方法は、別リポジトリ [env-sensor-board](https://github.com/ynug/env-sensor-board)
にあります（M5Stack Tab5 用の [tab5-env-monitor](https://github.com/ynug/tab5-env-monitor) と共通の基板）。

### 配線

基板は **3.3V 専用**で、Qwiic コネクタが付いています。Raspberry Pi には変換基板などは使わず、
Qwiic とジャンパーワイヤー（メス）の変換ケーブルで 40 ピンヘッダーに直接つなぎます。

| 基板の Qwiic | 線の色（Qwiic 標準） | Raspberry Pi |
|---|---|---|
| 1 GND | 黒 | 6 番ピン（GND） |
| 2 3.3V | 赤 | **1 番ピン（3.3V）** |
| 3 SDA | 青 | 3 番ピン（GPIO2 / SDA） |
| 4 SCL | 黄 | 5 番ピン（GPIO3 / SCL） |

- 赤の線を 2 番・4 番ピン（5V）につながないこと。BMP581 などの定格を超えて壊れます。
- SDA / SCL は基板側で 4.7kΩ にプルアップ済み。Raspberry Pi 側の 1.8kΩ と並列になりますが、そのまま使えます。

## 2. 設定

```toml
[sensors]
bus = "/dev/i2c-1"          # Raspberry Pi の GPIO2 (SDA) / GPIO3 (SCL)
interval_seconds = 1        # 読み取り間隔（1〜60）。SCD41 は 5 秒ごとにしか新しい値が出ない
stale_seconds = 60          # これより古い値は「古いデータ」として薄く表示
co2_caution_ppm = 1000      # 注意色（換気を促す）
co2_warning_ppm = 1500      # 警告色

[[sensors.device]]
id = "room"                 # 任意の名前（英数字と - _）。display から参照する
model = "sht35"
address = 0x44

[[sensors.device]]
id = "co2"
model = "scd41"
address = 0x62
pressure_from = "baro"      # 任意: この id のセンサーの気圧で CO2 を補正する
auto_calibration = true     # 任意: 自動校正 (ASC)。既定はセンサー側の設定のまま

[[sensors.device]]
id = "baro"
model = "bmp581"
address = 0x47

# どの値をどのセンサーから表示するか。書かなかった項目は表示しない。
[sensors.display]
temperature = "room"
humidity = "room"
co2 = "co2"
pressure = "baro"
```

`home-display --check-config` で、次の誤りを起動前に検出します。

- 知らない `model`、範囲外のアドレス（`0x08`〜`0x77` 以外）
- 同じバスでのアドレスの重複、`id` の重複
- `display` / `pressure_from` が存在しない `id`、またはその値を測れないセンサーを指している
  （例: `co2 = "room"` は SHT35 が CO2 を測れないのでエラー）

## 3. 表示

天気カードの下に「室内」として表示します。`display` に書いた項目だけを並べます。

| 画面 | 表示 |
|---|---|
| 雨雲レーダー | 現在の天気カードの「降水確率」と「降水の強さ」凡例の間に 2×2 で表示 |
| Overview | 天気カードの最下段（日の出・日の入の行の上）に 1 行で表示 |
| 設定・診断 | 全センサーの全測定値・アドレス・最終成功時刻・直近のエラー |

サンプルデータ（`mise run dev` や `--fixture`）では、`[sensors]` がなくても本機の基板の見本の値を表示します
（`rain` は CO2 が警告色、`long` は注意色、`stale` は古い値として薄く表示）。

- 表示の精度: 温度 0.1 ℃、湿度 1 %、CO2 1 ppm、気圧 1 hPa。
- CO2 が `co2_caution_ppm` 以上で注意色、`co2_warning_ppm` 以上で警告色（両画面とも）。
- `stale_seconds` を超えて更新されない値は薄く表示し、センサーが一度も読めていなければ「--」。
- データソースの状態一覧には「室内センサー」として 1 項目で出ます（1 台でも読めていれば正常、一部が失敗中なら再試行中）。

## 4. 動作

```
[I2C スレッド]（バス 1 本につき 1 本。Tokio / UI スレッドとは別）
   interval ごとに各センサーを読む → 共有の最新値（Arc<Mutex<…>>）を更新
   正常 ↔ エラーの変化があったときだけ Repository に状態を通知
[UI スレッド]
   毎秒の tick で最新値を読み、表示内容が変わったときだけ Slint に反映
```

- I2C の読み取りはブロッキングなので専用スレッドで行います。1 秒ごとに読んでも、
  他のパネルの再計算（`Controller::on_snapshot`）は起きません。雨雲の進捗（`radar::Progress`）と同じ方式です。
- センサーごとに独立して扱い、1 台が外れても他は読み続けます。
- 失敗が続いたセンサーは間隔を空けて（最大 60 秒）再初期化を試みます。バスが開けないときも同様です。
- SCD41 は本体の再起動をまたいで計測を続けているため、起動時は必ず停止してから設定し直します。

## 5. センサードライバーの追加方針

### 置き場所と形

```
src/sensors/
  mod.rs      設定 → ドライバーの生成、I2C スレッド、共有の最新値
  crc.rs      Sensirion 共通の CRC-8
  sht3x.rs
  scd4x.rs
  bmp581.rs
```

- ドライバーは **`embedded-hal` 1.0 の `I2c` trait** に対して書きます。Linux の `/dev/i2c-*` には
  `linux-embedded-hal` の `I2cdev` を渡します。これにより、ドライバーは macOS でも偽の I2C を使ってテストできます。
- ドライバーは共通の trait（仮称 `Sensor`）を実装します。
  - `init()` … リセット・チップ ID の確認・計測モードの設定
  - `read()` … 測定値（下記の単位）を返す。新しい値がまだなければ「値なし」を返す（エラーではない）
  - `quantities()` … 測れる値の種類（config の検証に使う）
- 遅延（コマンド後の待ち時間）はドライバー内で `std::thread::sleep` してよい（専用スレッドで動くため）。

### 必ず守ること

| 項目 | 内容 |
|---|---|
| 単位 | 温度 ℃、湿度 %RH、CO2 ppm、気圧 hPa に揃える（`f32`）。換算はドライバー内で行う |
| データの検証 | CRC があるセンサーは必ず確認する。チップ ID があるセンサーは `init()` で照合する |
| 範囲の確認 | 物理的にあり得ない値（湿度 0〜100 % の外、気圧 300〜1250 hPa の外など）はエラーとして捨てる |
| 待ち時間 | データシートの最大値を使う（標準値ではなく） |
| 書き込み寿命 | EEPROM に書くコマンド（SCD41 の `persist_settings` など）は使わない |
| テスト | 偽の I2C で「正常値の換算」「CRC 不一致」「チップ ID 不一致」を最低限テストする |

### 追加の手順

1. `src/sensors/<型番>.rs` にドライバーを書き、テストを付ける
2. `model` 名を設定の型に追加し、測れる値の種類を登録する
3. この文書の「対応センサー」表と、各センサーのメモ（下記）を更新する
4. fixture に必要ならサンプル値を追加する

## 6. ライセンスの方針

本プロジェクトは MIT ライセンスです。

- 依存 crate は **MIT / Apache-2.0 / BSD 系** に限ります。
  - `linux-embedded-hal`（MIT/Apache-2.0）は `default-features = false, features = ["i2c"]` で使い、
    シリアル・GPIO・SPI 用の依存を入れません。
  - `i2cdev`（MIT/Apache-2.0）、`nix`（MIT）、`embedded-hal`（MIT/Apache-2.0）
- ドライバーは **メーカーのデータシートを元に書きます。** GPL などのコピーレフトのドライバー
  （例: `bmp5xx` crate は GPL-3.0）のコードは参照・流用しません。
- メーカー公式のサンプルコード（例: Bosch の BMP5 Sensor API、BSD-3-Clause）を参考にするのは構いませんが、
  コードを写す場合はそのライセンス表示を残してください。

## 7. 各センサーのメモ

以下はデータシートからの要点です。実装時はデータシートの最新版と照合してください。

### SHT35（SHT3x）

- シングルショット計測（クロックストレッチなし・高精度）: コマンド `0x2400`、最大 15.5 ms 待って 6 バイト読む
  （温度 2 バイト + CRC、湿度 2 バイト + CRC）。
- 換算: 温度 = −45 + 175 × S / 65535、湿度 = 100 × S / 65535。
- CRC-8: 多項式 `0x31`、初期値 `0xFF`、反転なし（例: `0xBEEF` → `0x92`）。SCD41 と共通。
- 1 秒間隔の高精度計測では動作時間が 2 % 未満で、自己発熱の影響は小さい。

### SCD41

- 起動時: `stop_periodic_measurement`（`0x3F86`）→ 500 ms 待つ → 設定 → `start_periodic_measurement`（`0x21B1`）。
- 読み取り: `get_data_ready_status`（`0xE4B8`）で準備完了を確認してから `read_measurement`（`0xEC05`）。
  9 バイト（CO2・温度・湿度、各 2 バイト + CRC）。CO2 はそのまま ppm、温度・湿度の換算は SHT3x と同じ。
- 新しい値は 5 秒ごと。最初の値は計測開始から約 5 秒後。
- 気圧補正: `set_ambient_pressure`（`0xE000`、引数は hPa）を計測中に送れる。`pressure_from` の値を毎回送る。
- 自動校正（ASC）は、少なくとも週に一度は外気程度（約 400 ppm）まで換気される前提。換気の少ない部屋では無効を検討する。
  設定は停止中にしか変えられず、EEPROM には保存しない（起動ごとに設定する）。
- 自己発熱で温度が高めに出るため、温度・湿度の表示には SHT35 を使う想定。

### BMP581

- チップ ID（レジスタ `0x01`）が `0x50` であることを確認する。
- 強制計測（forced）モードで、読み取りのたびに 1 回計測させる。
- 温度: `0x1D`〜`0x1F`（24 ビット符号付き、÷ 65536 で ℃）。気圧: `0x20`〜`0x22`（24 ビット、÷ 64 で Pa、÷ 100 で hPa）。
- 気圧の計測は OSR 設定の `press_en` を有効にしないと値が出ない。

### 配置

- Raspberry Pi 4 とディスプレイ背面は発熱するため、温度・湿度センサーは少し離すかケーブルで外に出す。

## 8. Raspberry Pi の準備と権限

`install-raspberry-pi.sh` が次をまとめて行います（質問に「はい」、または `--sensors`）。

- I2C を有効にする（`raspi-config nonint do_i2c 0`。`raspi-config` がなければ `config.txt` に `dtparam=i2c_arm=on`
  と `i2c-dev` の自動読み込み）。`/dev/i2c-1` がまだなければ再起動を促します。
- `i2c-tools` を入れる（`i2cdetect` で配線を確認するため。`.deb` の推奨パッケージでもあります）。
- `config.toml` に上の基板の `[sensors]` を追加する（すでに `[sensors]` があれば変えません）。

`.deb` 側の設定:

- systemd ユニットは `DeviceAllow=char-i2c rw` と `SupplementaryGroups=i2c` を持ちます。
  本体が開けるデバイスは DRM・input・I2C だけで、他の制限は変えません。
- `i2c` グループがない環境（Raspberry Pi OS 以外の Debian など）では、インストール時に作成し、
  udev ルール（`/usr/lib/udev/rules.d/60-home-display-i2c.rules`）で `/dev/i2c-*` をこのグループに割り当てます。

### 接続後の確認

```sh
sudo i2cdetect -y 1                       # 0x44 / 0x47 / 0x62 が表示されれば配線 OK
sudo systemctl stop home-display          # 読み取りが重ならないように止める（SCD41 は起動時に設定し直すため）
sudo home-display --config /etc/home-display/config.toml --read-sensors   # 15 秒間読んで表示
sudo systemctl start home-display
```

画面では、設定画面（`S`）の「室内センサー」欄にセンサーごとの値・最終成功時刻・エラーが出ます。
センサー未接続のまま `[sensors]` を設定した場合は、天気カードの値が「--」になり、設定画面にエラーが表示されます。
