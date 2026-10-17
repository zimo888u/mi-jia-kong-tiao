# 千恋万花主题素材

- 用途：米家空调的可选「丛雨·亮色」和「丛雨·暗色」主题。
- murasame.png 和 character-atlas.png 生成工具：内置 imagegen，2026-10-14。
- murasame-standing.png：用户提供的丛雨透明立绘，2026-10-14，直接复制原始 PNG，未重绘。程序只在显示时裁去透明留白，并将立绘放在所有功能页的右侧背景。
- murasame.png：丛雨背景人物，透明 PNG，固定显示于所有页面的背景留白区。
- character-atlas.png：3 列 × 2 行的透明 Q 版头像图集。原图保持不变，程序按图标的实际物理像素尺寸取图并做面积加权滤波，随 Windows 缩放调整尺寸，避免软件渲染器的再次缩放造成锯齿。
- 丛雨背景图和主题缩略图分别按所在区域尺寸绘制，透明边缘保留抗锯齿过渡；缩小使用面积滤波，放大使用预乘色插值；绘制结果缓存复用。
- 图集顺序：朝武芳乃、常陆茉子、蕾娜；鞍马小春、马庭芦花、鞍马廉太郎。
- 角色参考：[千恋＊万花官方角色页](https://www.yuzu-soft.com/products/senren/chara.html)。角色所属：YUZUSOFT / JUNOS inc.
- Q 版图片为本地主题新生成的同人插画，站立立绘由用户提供；程序内嵌最终图像，原始官网参考图仅保存在 target 中，未打包。

## 丛雨提示词

```text
Use case: stylized-concept. Asset type: one transparent PNG mascot cutout for a Windows air-conditioner app theme inspired by Senren Banka (千恋万花).
Reference image 1 is the official neutral costume sheet of Murasame (丛雨 / ムラサメ), provided ONLY to identify hair, accessories, colors and clothes. Create a NEW wholesome chibi fan illustration, not a screenshot or a UI mockup.
Draw Murasame alone, entire body visible, very cute 2.5-head-tall proportions, large head, gentle happy expression, one hand waving. Pale mint-green long hair, two small side ponytail buns and long loose hair; plum-purple butterfly hairpin, fine magenta hair ribbons and gold dangling hair decoration; rosy red/pink eyes. Retain recognizable dark aubergine mini kimono, red-and-gold obi, long loose kimono sleeves with red floral lining, dark calf wraps and simple sandals. Keep clothes neat, simplify tiny patterns for UI clarity.
Style: polished Japanese anime SD sticker illustration, clean rounded outlines, soft cel shading, restrained pastel highlights, readable silhouette, cohesive plum/mint/sakura palette. Portrait 1024 x 1536 canvas, centered character occupies most of canvas with 8% transparent padding all around, nothing cropped. Truly transparent alpha background. No extra people, no scene, no floor, no cast shadow rectangle, no lettering, logos, watermarks or interface.
```

## 图集提示词

```text
Use case: stylized-concept. Asset type: ONE transparent sprite-atlas PNG for navigation icons in a Senren Banka (千恋万花) themed app.
Reference images 1 through 5 are official small character portraits of the five female characters, ONLY identity references. The sixth male character is described below. Produce a NEW matching chibi fan-art atlas rather than copying the portraits. A landscape 1536 x 1024 canvas, precisely THREE equal columns and TWO equal rows. Each 512 x 512 cell contains exactly ONE separate centered chibi head-and-upper-body sticker; leave at least 10% transparent padding inside every cell, all hair and accessories inside its own cell. No visible grid, borders, background, labels or text. Exactly six distinct people:
Top-left: Tomotake Yoshino / 朝武芳乃, silver-white hair, violet eyes, ornate golden shrine hair ornaments with red ties, white shrine outfit with red accents, calm soft smile.
Top-middle: Hitachi Mako / 常陆茉子, straight dark black-brown short bob, green eyes, small golden hair clip, dark ninja/shrine outfit, playful expression.
Top-right: Lena Lichtenauer / 蕾娜, long blonde hair, violet eyes, pale academy outfit with blue trim, cheerful smile.
Bottom-left: Kurama Koharu / 鞍马小春, orange/light-auburn bob, yellow-green eyes, dark green ribbon, pale school uniform, a gentle grin.
Bottom-middle: Maniwa Roka / 马庭芦花, pinkish dusty rose hair, brown eyes, soft side curls, white-and-plum café outfit, friendly expression.
Bottom-right: Kurama Rentaro / 鞍马廉太郎, short light-brown hair, golden-brown eyes, white shirt and dark academy blazer, bright smile.
Style: consistent kawaii Japanese SD anime stickers, oversized heads, tiny shoulders and hands, clean rounded plum outlines, soft cel shading, compact clear shapes that remain legible at 36px. Truly transparent alpha background. No Murasame in this atlas (she is the separate background mascot). Keep row/column ordering exact, no mixed panels, no extra characters, no logos or watermark.
```
