# QMXPan

**QRP Labs QMX / QMX+** için düşük maliyetli bir panadaptör. **Waveshare
ESP32-S3-Touch-LCD-4.3** (800×480 dokunmatik ekran) üzerinde çalışır. Kart QMX'in USB
portuna takılır; I/Q sesini ve CAT portunu okuyup canlı spektrum ve şelale gösterir.
Spektruma dokununca telsiz o frekansa ayarlanır.

*English: [README.md](README.md)*

## Özellikler

| | Full | Lite |
|---|:-:|:-:|
| Spektrum + şelale, ×1…×16 büyütme, dokun/sürükle ile ayar | ✓ | ✓ |
| Bant düğmeleri ve bant başına frekans hafızası; mod çipleri (LSB/USB/CW/DIGI) | ✓ | ✓ |
| CW çözücü, FT8 çözücü (liste ve şelale etiketleri) | ✓ | ✓ |
| METERS sayfası: ibreli SWR / S-metre / güç, Nixie görünümlü saat | ✓ | ✓ |
| UTC saati | internetten (NTP) | elle kurulur, FT8 ile kendiliğinden düzeltilir |
| WiFi, web ayar sayfası, WiFi'den yazılım güncelleme | ✓ | – |
| Spektrumda DX cluster spotları | ✓ | – |
| PROPAGATION sayfası (güneş verisi, gece/gündüz haritası) | ✓ | – |

**Hangisini seçmeli?**
- Çalıştığınız yerde WiFi varsa **Full**.
- Portatif ya da internetsiz kullanımda **Lite**.

Lite hiç WiFi başlatmaz. Bu sayede daha çok bellek boş kalır ve açılış daha sadedir.

## Gerekenler

- I/Q modu olan (CAT komutu `Q9`) bir yazılım yüklü QRP Labs QMX ya da QMX+.
- Waveshare **ESP32-S3-Touch-LCD-4.3** (16 MB flash / 8 MB PSRAM sürümü).
- USB kablosu: kartın **USB OTG** portu QMX'in USB portuna bağlanır. Kartın diğer USB
  portu (UART) yalnız yazılım yüklemek için kullanılır.
- Kart için 5 V besleme.

> **QMX+ sahipleri:** QMX+ için olan yazılımı kullanın. Düz QMX yazılımı QMX+ üzerinde bazı
> bantlara geçmez; QMXPan o zaman *"QMX: xx m not available"* uyarısı gösterir.

## Yükleme

### En kolayı: tarayıcıdan

1. Bilgisayarda Chrome ya da Edge ile **[QMXPan yükleme sayfasını](https://OWNER.github.io/QMXPan/)** açın.
2. Kartın **UART** USB portunu bilgisayara bağlayın.
3. **Install Full** ya da **Install Lite** düğmesine basın, seri portu seçip onaylayın.

İlk yüklemede **Erase device** kutusunu işaretleyin. Güncellemede işaretlemeyin; böylece
WiFi, çağrı işareti ve ayarlar korunur.

### Elle

[Releases](https://github.com/OWNER/QMXPan/releases) sayfasından `qmxpan-full-<sürüm>.bin`
ya da `qmxpan-lite-<sürüm>.bin` dosyasını indirip `0x0` adresine yazın:

```bash
espflash write-bin 0x0 qmxpan-full-0.14.0.bin
```

Bu tek parça imaj kayıtlı ayarları da siler.

## İlk açılış

1. Kartı QMX'e bağlayın, sonra kartı besleyin.
2. **QMX'in 12 V'unu bir kez kapatıp açın.** QMX ancak böyle USB'de görünür. Kart her yeniden
   başladığında da aynısını yapın. QMXPan USB portunu kendisi yeniden dener.
3. **Full:** kart `QMXPan-Setup` adlı bir WiFi ağı açar.
   1. Bu ağa bağlanın.
   2. `http://192.168.71.1` adresini açın.
   3. Evdeki WiFi ağınızın adını ve şifresini girin.
   4. Kart yeniden başlayınca IP adresi ekranın sağ üstünde görünür. O adresten şu ayarlar yapılır:
      - çağrı işareti ve DX cluster
      - harita için konum kodu (locator)
      - S-metre kalibrasyonu
      - yazılım güncelleme
4. **Lite (ya da WiFi'siz Full):**
   1. Ekranın üstündeki **UTC** yazısına dokunun.
   2. Çağrı işaretinizi yazın.
   3. Doğru bir saatin *bir sonraki* dakikasını ayarlayın.
   4. O saatin saniyesi tam :00 olduğunda **SET CLOCK**'a basın.

   Sonrasında FT8 çözümleri saati kendiliğinden düzeltir.

Aynı pencere **DISPLAY → Callsign & clock** yolundan da açılır.

## Kullanım

- Ayar için spektruma ya da şelaleye **dokunun veya sürükleyin**. FT8 ya da DX etiketine kısa dokunuş o istasyona ayarlar.
- **Frekansa** dokunmak (ya da **BAND**) bant düğmelerini ve **DISPLAY** ayarlarını açar.
- **ZOOM** sırayla ×1 → ×2 → ×4 → ×8 → ×16 yapar.
- **FT8** sırayla şunlar arasında geçer: liste → etiketli şelale → kapalı. Şelale görünümü FT8 bölgesine yaklaşır.
- **CW** CW çözücüyü gösterir.
- **MENU** METERS sayfasını, Full'da ayrıca PROPAGATION sayfasını açar.

## Güncelleme

- **Full:** kartın web sayfasında `qmxpan-full-ota-<sürüm>.bin` dosyasını seçip **Upload**'a basın.
  İlk deneme bağlantıyı koparırsa sayfa kendiliğinden yeniden dener.
- **Lite:** yükleme sayfasını yeniden kullanın, *Erase device* işaretsiz kalsın.

## Kaynaktan derleme

- Rust `esp` araç zinciri ([espup](https://github.com/esp-rs/espup)), `ldproxy` ve `espflash` gerekir.
- ESP-IDF v5.4.2'yi `esp-idf-sys` kendisi indirir.

```bash
cargo build --release
```

```bash
cargo build --release --no-default-features
```

İlk komut Full'ü, ikincisi Lite'ı derler. `tools/package.ps1` iki paketi de derleyip
tüm sürüm dosyalarını `dist/` altına yazar.

> `sdkconfig.defaults` içinde `partitions.csv` için mutlak bir yol var
> (`CONFIG_PARTITION_TABLE_CUSTOM_FILENAME`). Bunu kendi klasörünüze göre değiştirin.

## Teşekkürler

- [ft8_lib](https://github.com/kgoba/ft8_lib), Karlis Goba (MIT): FT8 çözme.
- Güneş verisi: [N0NBH, hamqsl.com](https://www.hamqsl.com/solar.html) (yalnız Full).
- Harita: [Natural Earth](https://www.naturalearthdata.com/) (kamu malı).
- Yazı tipleri: Source Sans Pro (SIL Open Font License 1.1).
- Espressif ESP-IDF ve esp-rs kütüphaneleri.

QMX ve QMX+ QRP Labs ürünleridir. QMXPan bağımsız bir projedir ve QRP Labs ile bağlantısı yoktur.

## Lisans

LICENSE_TBD
