// Copyright 2018 the Resvg Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

pub fn render(
    image: &usvg::Image,
    transform: tiny_skia::Transform,
    pixmap: &mut tiny_skia::PixmapMut,
) {
    if !image.is_visible() {
        return;
    }

    render_inner(image.kind(), transform, image.rendering_mode(), pixmap);
}

pub fn render_inner(
    image_kind: &usvg::ImageKind,
    transform: tiny_skia::Transform,
    #[allow(unused_variables)] rendering_mode: usvg::ImageRendering,
    pixmap: &mut tiny_skia::PixmapMut,
) {
    match image_kind {
        usvg::ImageKind::SVG(tree) => {
            render_vector(tree, transform, pixmap);
        }
        #[cfg(feature = "raster-images")]
        _ => {
            raster_images::render_raster(image_kind, transform, rendering_mode, pixmap);
        }
        #[cfg(not(feature = "raster-images"))]
        _ => {
            log::warn!("Images decoding was disabled by a build feature.");
        }
    }
}

fn render_vector(
    tree: &usvg::Tree,
    transform: tiny_skia::Transform,
    pixmap: &mut tiny_skia::PixmapMut,
) -> Option<()> {
    let mut sub_pixmap = tiny_skia::Pixmap::new(pixmap.width(), pixmap.height()).unwrap();
    crate::render(tree, transform, &mut sub_pixmap.as_mut());
    pixmap.draw_pixmap(
        0,
        0,
        sub_pixmap.as_ref(),
        &tiny_skia::PixmapPaint::default(),
        tiny_skia::Transform::default(),
        None,
    );

    Some(())
}

#[cfg(feature = "raster-images")]
mod raster_images {
    use crate::OptionLog;
    use std::io::Cursor;
    use usvg::ImageRendering;

    fn decode_raster(image: &usvg::ImageKind) -> Option<tiny_skia::Pixmap> {
        match image {
            usvg::ImageKind::SVG(_) => None,
            usvg::ImageKind::JPEG(data) => {
                decode_jpeg(data).log_none(|| log::warn!("Failed to decode a JPEG image."))
            }
            usvg::ImageKind::PNG(data) => {
                decode_png(data).log_none(|| log::warn!("Failed to decode a PNG image."))
            }
            usvg::ImageKind::GIF(data) => {
                decode_gif(data).log_none(|| log::warn!("Failed to decode a GIF image."))
            }
            usvg::ImageKind::WEBP(data) => {
                decode_webp(data).log_none(|| log::warn!("Failed to decode a WebP image."))
            }
        }
    }

    fn decode_png(data: &[u8]) -> Option<tiny_skia::Pixmap> {
        tiny_skia::Pixmap::decode_png(data).ok()
    }

    fn decode_jpeg(data: &[u8]) -> Option<tiny_skia::Pixmap> {
        use zune_jpeg::zune_core::colorspace::ColorSpace;
        use zune_jpeg::zune_core::options::DecoderOptions;

        let cursor = Cursor::new(data);
        let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
        let mut decoder = zune_jpeg::JpegDecoder::new_with_options(cursor, options);
        decoder.decode_headers().ok()?;
        let output_cs = decoder.output_colorspace()?;

        let img_data = {
            let data = decoder.decode().ok()?;
            match output_cs {
                ColorSpace::RGBA => data,
                _ => return None,
            }
        };

        let info = decoder.info()?;

        let size = tiny_skia::IntSize::from_wh(info.width as u32, info.height as u32)?;
        tiny_skia::Pixmap::from_vec(img_data, size)
    }

    fn decode_gif(data: &[u8]) -> Option<tiny_skia::Pixmap> {
        let mut decoder = gif::DecodeOptions::new();
        decoder.set_color_output(gif::ColorOutput::RGBA);
        let mut decoder = decoder.read_info(data).ok()?;
        let first_frame = decoder.read_next_frame().ok()??;

        let size = tiny_skia::IntSize::from_wh(
            u32::from(first_frame.width),
            u32::from(first_frame.height),
        )?;

        let (w, h) = size.dimensions();
        let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
        rgba_to_pixmap(&first_frame.buffer, &mut pixmap);
        Some(pixmap)
    }

    fn decode_webp(data: &[u8]) -> Option<tiny_skia::Pixmap> {
        let mut decoder = image_webp::WebPDecoder::new(std::io::Cursor::new(data)).ok()?;
        let mut first_frame = vec![0; decoder.output_buffer_size()?];
        decoder.read_image(&mut first_frame).ok()?;

        let (w, h) = decoder.dimensions();
        let mut pixmap = tiny_skia::Pixmap::new(w, h)?;

        if decoder.has_alpha() {
            rgba_to_pixmap(&first_frame, &mut pixmap);
        } else {
            rgb_to_pixmap(&first_frame, &mut pixmap);
        }

        Some(pixmap)
    }

    fn rgb_to_pixmap(data: &[u8], pixmap: &mut tiny_skia::Pixmap) {
        use rgb::FromSlice;

        let mut i = 0;
        let dst = pixmap.data_mut();
        for p in data.as_rgb() {
            dst[i + 0] = p.r;
            dst[i + 1] = p.g;
            dst[i + 2] = p.b;
            dst[i + 3] = 255;

            i += tiny_skia::BYTES_PER_PIXEL;
        }
    }

    fn rgba_to_pixmap(data: &[u8], pixmap: &mut tiny_skia::Pixmap) {
        use rgb::FromSlice;

        let mut i = 0;
        let dst = pixmap.data_mut();
        for p in data.as_rgba() {
            let a = p.a as f64 / 255.0;
            dst[i + 0] = (p.r as f64 * a + 0.5) as u8;
            dst[i + 1] = (p.g as f64 * a + 0.5) as u8;
            dst[i + 2] = (p.b as f64 * a + 0.5) as u8;
            dst[i + 3] = p.a;

            i += tiny_skia::BYTES_PER_PIXEL;
        }
    }

    pub(crate) fn render_raster(
        image: &usvg::ImageKind,
        transform: tiny_skia::Transform,
        rendering_mode: usvg::ImageRendering,
        pixmap: &mut tiny_skia::PixmapMut,
    ) -> Option<()> {
        let raster = decode_raster(image)?;

        // -----------------------------------------------------------------
        // 1.  Figure out how big the image will be in destination pixels.
        //     For an affine transform | sx  kx  tx |
        //                             | ky  sy  ty |
        //     the column lengths give the approximate x/y scale factors.
        // -----------------------------------------------------------------
        let scale_x = (transform.sx * transform.sx + transform.ky * transform.ky).sqrt();
        let scale_y = (transform.kx * transform.kx + transform.sy * transform.sy).sqrt();

        let src_w = raster.width() as f32;
        let src_h = raster.height() as f32;
        let dst_w = (src_w * scale_x).ceil().max(1.0) as u32;
        let dst_h = (src_h * scale_y).ceil().max(1.0) as u32;

        // -----------------------------------------------------------------
        // 2.  If we are down-scaling by more than 2×, pre-scale with a box
        //     filter so tiny_skia's bicubic only ever sees ≤2× downscale.
        // -----------------------------------------------------------------
        let (raster, transform) =
            if dst_w < src_w as u32 / 2 || dst_h < src_h as u32 / 2 &&
                (rendering_mode == ImageRendering::OptimizeQuality ||
                 rendering_mode == ImageRendering::Smooth ||
                 rendering_mode == ImageRendering::HighQuality) {
            // Keep the intermediate image 2× the final size so bicubic has
            // enough samples to work with.
            let target_w = (dst_w * 2).min(src_w as u32).max(1);
            let target_h = (dst_h * 2).min(src_h as u32).max(1);

            let scaled = box_filter_resize(&raster, target_w, target_h)?;

            // The new image is smaller; we must scale local coordinates back
            // up so the image still covers the same destination rectangle.
            // T' = T * scale(src/target)  →  post_scale
            let up_x = src_w / target_w as f32;
            let up_y = src_h / target_h as f32;
            let adjusted = transform.pre_scale(up_x, up_y);

            (scaled, adjusted)
        } else {
            (raster, transform)
        };

        // -----------------------------------------------------------------
        // 3.  Original tiny_skia rendering path (unchanged).
        // -----------------------------------------------------------------
        let rect = tiny_skia::Size::from_wh(raster.width() as f32, raster.height() as f32)?
            .to_rect(0.0, 0.0)?;

        let quality = match rendering_mode {
            ImageRendering::OptimizeQuality => tiny_skia::FilterQuality::Bicubic,
            ImageRendering::OptimizeSpeed => tiny_skia::FilterQuality::Nearest,
            ImageRendering::Smooth => tiny_skia::FilterQuality::Bilinear,
            ImageRendering::HighQuality => tiny_skia::FilterQuality::Bicubic,
            ImageRendering::CrispEdges => tiny_skia::FilterQuality::Nearest,
            ImageRendering::Pixelated => tiny_skia::FilterQuality::Nearest,
        };

        let pattern = tiny_skia::Pattern::new(
            raster.as_ref(),
            tiny_skia::SpreadMode::Pad,
            quality,
            1.0,
            tiny_skia::Transform::default(),
        );
        let mut paint = tiny_skia::Paint::default();
        paint.shader = pattern;

        pixmap.fill_rect(rect, &paint, transform, None);

        Some(())
    }

    /// Box-filter resize. Averages all source pixels that fall inside each
    /// destination pixel's footprint. This is exactly the low-pass filter
    /// that a mipmap level-1 would apply.
    fn box_filter_resize(
        src: &tiny_skia::Pixmap,
        dst_w: u32,
        dst_h: u32,
    ) -> Option<tiny_skia::Pixmap> {
        let src_w = src.width();
        let src_h = src.height();
        let mut dst = tiny_skia::Pixmap::new(dst_w, dst_h)?;

        let src_data = src.data();
        let dst_data = dst.data_mut();

        let x_ratio = src_w as f32 / dst_w as f32;
        let y_ratio = src_h as f32 / dst_h as f32;

        for dy in 0..dst_h {
            let y0 = (dy as f32 * y_ratio) as u32;
            let y1 = (((dy + 1) as f32 * y_ratio).ceil() as u32).min(src_h);

            for dx in 0..dst_w {
                let x0 = (dx as f32 * x_ratio) as u32;
                let x1 = (((dx + 1) as f32 * x_ratio).ceil() as u32).min(src_w);

                let mut r: u64 = 0;
                let mut g: u64 = 0;
                let mut b: u64 = 0;
                let mut a: u64 = 0;
                let mut count: u64 = 0;

                for sy in y0..y1 {
                    let row = (sy * src_w) as usize * 4;
                    for sx in x0..x1 {
                        let i = row + (sx as usize * 4);
                        r += src_data[i]     as u64;
                        g += src_data[i + 1] as u64;
                        b += src_data[i + 2] as u64;
                        a += src_data[i + 3] as u64;
                        count += 1;
                    }
                }

                let i = ((dy * dst_w + dx) as usize) * 4;
                if count > 0 {
                    dst_data[i]     = (r / count) as u8;
                    dst_data[i + 1] = (g / count) as u8;
                    dst_data[i + 2] = (b / count) as u8;
                    dst_data[i + 3] = (a / count) as u8;
                }
            }
        }

        Some(dst)
    }
}
