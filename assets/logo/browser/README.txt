OPENHERONBROWSER LOGO

A heron hatching from a cracked wireframe egg: the egg is the web globe, the
heron is Heronetic's (same head, beak and plume as the Heronetic mark), and the
top of the shell is tipping away as it breaks off. Gold #F5C842, black #0B0B0C.

SQUARE (rounded-square tile, the app icon)
  ohb-icon.svg            64 px and up
  ohb-icon-small.svg      32-48 px (fewer wires, no shell piece)
  ohb-favicon.svg         16-24 px
  ohb.icns                macOS app icon, 16-1024 px (small sizes use the simpler art)
  ohb-favicon.ico         website favicon, 16 and 32 px

ROUND (circle: profile pictures, round icons)
  ohb-round.svg           64 px and up
  ohb-round-small.svg     32-48 px
  ohb-round-favicon.svg   16-24 px

  png/                    PNG exports of both shapes at common sizes
  make_logo.py            regenerates the SVGs (python3 make_logo.py writes ./out/)

APP ICON
  Replace OpenHeronBrowser.app/Contents/Resources/ohb.icns with ohb.icns
  (Info.plist already points at it). If macOS keeps showing the old icon, move
  the app to another folder and back.
