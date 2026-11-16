# QMXPan: src/fonts.rs dosyasındaki yumuşatılmış (4 bit) fontları üretir.
# Kaynak: Source Sans Pro (SIL Open Font License 1.1), Windows ile gelir (C:\Windows\Fonts).
# QRP DDS projesindeki tools/fontgen.ps1'den uyarlandı; çıktı Rust.
#
#   powershell -ExecutionPolicy Bypass -File tools\fontgen.ps1
#
# Her glif satır satır 4 bit kaplama değeridir: bayt başına iki piksel, önce üst yarım
# bayt; her satır bayt sınırında başlar. ASCII dışı karakterler (Türkçe harfler, derece,
# orta nokta, eksi, uzun çizgi) kod noktası tablosundadır.

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$outFile = Join-Path $root 'src\fonts.rs'

Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Imaging;
using System.Drawing.Text;
using System.Runtime.InteropServices;
using System.Text;

public static class FontGenRs {
  public static string Make(string name, string ttf, float px, string chars, StringBuilder index) {
    var pfc = new PrivateFontCollection();
    pfc.AddFontFile(ttf);
    var fam = pfc.Families[0];
    FontStyle st = FontStyle.Regular;
    if (!fam.IsStyleAvailable(st)) st = FontStyle.Bold;
    var font = new Font(fam, px, st, GraphicsUnit.Pixel);
    float asc = px * fam.GetCellAscent(st) / fam.GetEmHeight(st);

    var cps = new List<int>();
    foreach (char ch in chars) if (!cps.Contains(ch)) cps.Add(ch);
    cps.Sort();

    var bytes = new List<byte>();
    var glyphs = new StringBuilder();
    int top = 0, bottom = 0;
    int W = (int)(px * 3), H = (int)(px * 2), ox = (int)px, oy = (int)(px * 0.3f);
    var fmt = (StringFormat)StringFormat.GenericTypographic.Clone();
    fmt.FormatFlags |= StringFormatFlags.MeasureTrailingSpaces;

    foreach (int cp in cps) {
      string s = ((char)cp).ToString();
      var bmp = new Bitmap(W, H, PixelFormat.Format32bppArgb);
      int adv;
      using (var g = Graphics.FromImage(bmp)) {
        g.Clear(Color.Black);
        g.TextRenderingHint = TextRenderingHint.AntiAliasGridFit;
        g.DrawString(s, font, Brushes.White, ox, oy, fmt);
        adv = (int)Math.Round(g.MeasureString(s, font, 1000, fmt).Width);
      }
      var data = bmp.LockBits(new Rectangle(0, 0, W, H), ImageLockMode.ReadOnly, PixelFormat.Format32bppArgb);
      var raw = new byte[data.Stride * H];
      Marshal.Copy(data.Scan0, raw, 0, raw.Length);
      bmp.UnlockBits(data);
      int stride = data.Stride;
      // Gamma 0.7: ince kenar pikselleri biraz kalınlaşır, koyu zeminde okunaklı kalır
      Func<int, int, int> lev = (x, y) => (int)Math.Round(15 * Math.Pow(raw[y * stride + x * 4 + 2] / 255.0, 0.7));

      int x0 = W, x1 = -1, y0 = H, y1 = -1;
      for (int y = 0; y < H; y++)
        for (int x = 0; x < W; x++)
          if (lev(x, y) > 0) { if (x < x0) x0 = x; if (x > x1) x1 = x; if (y < y0) y0 = y; if (y > y1) y1 = y; }
      int baseY = (int)Math.Round(oy + asc);
      int off = bytes.Count, w = 0, h = 0, xo = 0, yo = 0;
      if (x1 >= 0) {
        w = x1 - x0 + 1; h = y1 - y0 + 1; xo = x0 - ox; yo = y0 - baseY;
        for (int y = y0; y <= y1; y++)
          for (int x = x0; x <= x1; x += 2) {
            int a = lev(x, y), b = x + 1 <= x1 ? lev(x + 1, y) : 0;
            bytes.Add((byte)((a << 4) | b));
          }
        if (-yo > top) top = -yo;
        if (yo + h > bottom) bottom = yo + h;
      }
      glyphs.AppendFormat("    Glyph {{ off: {0}, w: {1}, h: {2}, xo: {3}, yo: {4}, adv: {5} }}, // U+{6:X4}\n", off, w, h, xo, yo, adv, cp);
      bmp.Dispose();
    }

    var sb = new StringBuilder();
    sb.AppendFormat("// {0}: {1} {2}px, {3} glif, {4} bayt\n", name, System.IO.Path.GetFileName(ttf), px, cps.Count, bytes.Count);
    sb.AppendFormat("pub static {0}: Font = Font {{\n  bmp: &[", name);
    for (int i = 0; i < bytes.Count; i++) {
      if (i % 24 == 0) sb.Append("\n    ");
      sb.AppendFormat("0x{0:X2},", bytes[i]);
    }
    sb.Append("\n  ],\n  gl: &[\n");
    sb.Append(glyphs);
    sb.Append("  ],\n  cp: &[");
    for (int i = 0; i < cps.Count; i++) { if (i % 16 == 0) sb.Append("\n    "); sb.AppendFormat("0x{0:X4},", cps[i]); }
    sb.AppendFormat("\n  ],\n  top: {0},\n  bottom: {1},\n}};\n\n", top, bottom);
    index.AppendFormat("// {0}: {1}px, {2} glif\n", name, px, cps.Count);
    return sb.ToString();
  }
}
'@

$fonts = 'C:\Windows\Fonts'
$reg = Join-Path $fonts 'SourceSansPro-Regular.ttf'
$bold = Join-Path $fonts 'SourceSansPro-Bold.ttf'

$ascii = -join (32..126 | ForEach-Object { [char]$_ })
# Ç ç Ğ ğ İ ı Ö ö Ş ş Ü ü ° · − — ☰ (kod noktası olarak; betiğin kodlaması önemsiz olsun)
$tr = -join (@(0xC7, 0xE7, 0x11E, 0x11F, 0x130, 0x131, 0xD6, 0xF6, 0x15E, 0x15F, 0xDC, 0xFC,
    0xB0, 0xB7, 0x2212, 0x2014) | ForEach-Object { [char]$_ })
$digits = ' 0123456789.'

$specs = @(
  @('F_R11', $reg, 11, ($ascii + $tr)),
  @('F_B12', $bold, 12, ($ascii + $tr)),
  @('F_B14', $bold, 14, ($ascii + $tr)),
  @('F_R14', $reg, 14, ($ascii + $tr)),
  @('F_B16', $bold, 16, ($ascii + $tr)),
  @('F_B30', $bold, 30, ($ascii + $tr)),
  @('F_D36', $bold, 36, $digits),
  @('F_D56', $bold, 56, $digits)
)

$index = New-Object System.Text.StringBuilder
$body = New-Object System.Text.StringBuilder
foreach ($s in $specs) {
  [void]$body.Append([FontGenRs]::Make($s[0], $s[1], $s[2], $s[3], $index))
}

$head = "// tools/fontgen.ps1 ile Source Sans Pro'dan (SIL OFL 1.1) üretildi. Elle düzenlemeyin.`n" +
  $index.ToString() + "`nuse crate::gfx::{Font, Glyph};`n`n"
$enc = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllText($outFile, $head + $body.ToString(), $enc)
Write-Host $index.ToString()
