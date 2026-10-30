# Génère assets/icon.ico : une grande fenêtre et trois petites cartes empilées sur la gauche,
# comme le symbole de Stage Manager. Relancer ce script après avoir modifié le dessin.
#   powershell -ExecutionPolicy Bypass -File assets/make-icon.ps1
Add-Type -AssemblyName System.Drawing

function RoundedRect([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
  $p = New-Object Drawing.Drawing2D.GraphicsPath
  $d = $r * 2
  $p.AddArc($x, $y, $d, $d, 180, 90)
  $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
  $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
  $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
  $p.CloseFigure()
  $p
}

# Dessine l'icône sur une grille de 256, mise à l'échelle pour chaque taille.
function Draw([int]$size) {
  $bmp = New-Object Drawing.Bitmap $size, $size, ([Drawing.Imaging.PixelFormat]::Format32bppArgb)
  $g = [Drawing.Graphics]::FromImage($bmp)
  $g.SmoothingMode = [Drawing.Drawing2D.SmoothingMode]::AntiAlias
  $g.PixelOffsetMode = [Drawing.Drawing2D.PixelOffsetMode]::HighQuality
  $g.ScaleTransform($size / 256.0, $size / 256.0)

  # Fond : carré arrondi en dégradé bleu → violet.
  $bg = RoundedRect 8 8 240 240 56
  $grad = New-Object Drawing.Drawing2D.LinearGradientBrush (New-Object Drawing.PointF 8, 8), (New-Object Drawing.PointF 248, 248), ([Drawing.Color]::FromArgb(255, 64, 132, 255)), ([Drawing.Color]::FromArgb(255, 132, 84, 240))
  $g.FillPath($grad, $bg)

  # Trois cartes empilées à gauche (les autres scènes).
  $card = New-Object Drawing.SolidBrush ([Drawing.Color]::FromArgb(150, 255, 255, 255))
  foreach ($y in 62, 112, 162) { $g.FillPath($card, (RoundedRect 36 $y 58 36 9)) }

  # Grande fenêtre à droite (la scène active), avec sa barre de titre.
  $win = RoundedRect 108 54 116 148 16
  $g.FillPath((New-Object Drawing.SolidBrush ([Drawing.Color]::FromArgb(250, 255, 255, 255))), $win)
  $g.SetClip($win)
  $g.FillRectangle((New-Object Drawing.SolidBrush ([Drawing.Color]::FromArgb(255, 205, 218, 255))), 108, 54, 116, 26)
  $g.ResetClip()

  $g.Dispose()
  $bmp
}

$sizes = 16, 20, 24, 32, 40, 48, 64, 256
$pngs = foreach ($s in $sizes) {
  $bmp = Draw $s
  $ms = New-Object IO.MemoryStream
  $bmp.Save($ms, [Drawing.Imaging.ImageFormat]::Png)
  $bmp.Dispose()
  , $ms.ToArray()
}

# Fichier .ico : en-tête, répertoire, puis les images PNG (accepté depuis Windows Vista).
$out = New-Object IO.MemoryStream
$w = New-Object IO.BinaryWriter $out
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
  $s = $sizes[$i]; $dim = if ($s -ge 256) { 0 } else { $s }
  $w.Write([byte]$dim); $w.Write([byte]$dim); $w.Write([byte]0); $w.Write([byte]0)
  $w.Write([uint16]1); $w.Write([uint16]32)
  $w.Write([uint32]$pngs[$i].Length); $w.Write([uint32]$offset)
  $offset += $pngs[$i].Length
}
foreach ($p in $pngs) { $w.Write($p) }
$w.Flush()
[IO.File]::WriteAllBytes((Join-Path $PSScriptRoot "icon.ico"), $out.ToArray())

# Aperçu pour vérifier le dessin.
$preview = Draw 256
$preview.Save((Join-Path $PSScriptRoot "icon-preview.png"), [Drawing.Imaging.ImageFormat]::Png)
$preview.Dispose()
