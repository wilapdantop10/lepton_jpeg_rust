/*---------------------------------------------------------------------------------------------
 *  Copyright (c) Microsoft Corporation. All rights reserved.
 *  Licensed under the Apache License, Version 2.0. See LICENSE.txt in the project root for license information.
 *  This software incorporates material from third parties. See NOTICE.txt for details.
 *--------------------------------------------------------------------------------------------*/

use log::info;

use crate::consts::{ALIGNED_BLOCK_INDEX_DC_INDEX, RASTER_TO_ALIGNED, ZIGZAG_TO_ALIGNED};

use super::{block_context::BlockContext, jpeg_header::JPegHeader};

/// holds the 8x8 blocks for a given component. Since we do multithreaded encoding,
/// the image may only hold a subset of the components (specified by dpos_offset),
/// but they can be merged
pub struct BlockBasedImage {
    block_width: i32,

    original_height: i32,

    dpos_offset: i32,

    image: Vec<AlignedBlock>,
}

static EMPTY: AlignedBlock = AlignedBlock { raw_data: [0; 64] };

impl BlockBasedImage {
    // constructs new block image for the given y-coordinate range
    pub fn new(
        jpeg_header: &JPegHeader,
        component: usize,
        luma_y_start: i32,
        luma_y_end: i32,
    ) -> Self {
        let block_width = jpeg_header.cmp_info[component].bch;
        let original_height = jpeg_header.cmp_info[component].bcv;
        let max_size = block_width * original_height;

        let image_capcity = usize::try_from(
            (i64::from(max_size) * i64::from(luma_y_end - luma_y_start)
                + i64::from(jpeg_header.cmp_info[0].bcv - 1 /* round up */))
                / i64::from(jpeg_header.cmp_info[0].bcv),
        )
        .unwrap();

        let dpos_offset = i32::try_from(
            i64::from(max_size) * i64::from(luma_y_start) / i64::from(jpeg_header.cmp_info[0].bcv),
        )
        .unwrap();

        return BlockBasedImage {
            block_width: block_width,
            original_height: original_height,
            image: Vec::with_capacity(image_capcity),
            dpos_offset: dpos_offset,
        };
    }

    /// merges a bunch of block images generated by different threads into a single one used by progressive decoding
    pub fn merge(images: &mut Vec<Vec<BlockBasedImage>>, index: usize) -> Self {
        // figure out the total size of all the blocks so we can set the capacity correctly
        let total_size = images.iter().map(|x| x[index].image.len()).sum();

        let mut contents = Vec::with_capacity(total_size);
        let mut block_width = None;
        let mut original_height = None;

        for v in images {
            assert!(
                v[index].dpos_offset == contents.len() as i32,
                "previous content should match new content"
            );

            if let Some(w) = block_width {
                assert_eq!(w, v[index].block_width, "all block_width must match")
            } else {
                block_width = Some(v[index].block_width);
            }

            if let Some(w) = original_height {
                assert_eq!(
                    w, v[index].original_height,
                    "all original_height must match"
                )
            } else {
                original_height = Some(v[index].original_height);
            }

            contents.append(&mut v[index].image);
        }

        return BlockBasedImage {
            block_width: block_width.unwrap(),
            original_height: original_height.unwrap(),
            image: contents,
            dpos_offset: 0,
        };
    }

    #[allow(dead_code)]
    pub fn dump(&self) {
        info!(
            "size = {0}, capacity = {1}, dpos_offset = {2}",
            self.image.len(),
            self.image.capacity(),
            self.dpos_offset
        );
    }

    pub fn off_y(&self, y: i32) -> BlockContext {
        return BlockContext::new(
            self.block_width * y,
            if y != 0 {
                self.block_width * (y - 1)
            } else {
                -1
            },
            if (y & 1) != 0 { self.block_width } else { 0 },
            if (y & 1) != 0 { 0 } else { self.block_width },
            self,
        );
    }

    pub fn get_block_width(&self) -> i32 {
        self.block_width
    }

    pub fn get_original_height(&self) -> i32 {
        self.original_height
    }

    fn fill_up_to_dpos(&mut self, dpos: i32) {
        // set our dpos the first time we get set, since we should be seeing our data in order
        if self.image.len() == 0 {
            assert!(self.dpos_offset == dpos);
        }

        assert!(dpos >= self.dpos_offset);

        while self.image.len() <= (dpos - self.dpos_offset) as usize {
            if self.image.len() >= self.image.capacity() {
                panic!("out of memory");
            }
            self.image.push(AlignedBlock { raw_data: [0; 64] });
        }
    }

    pub fn set_block_data(&mut self, dpos: i32, block_data: &[i16; 64]) {
        self.fill_up_to_dpos(dpos);
        self.image[(dpos - self.dpos_offset) as usize] = AlignedBlock {
            raw_data: *block_data,
        };
    }

    pub fn get_block(&self, dpos: i32) -> &AlignedBlock {
        if (dpos - self.dpos_offset) as usize >= self.image.len() {
            return &EMPTY;
        } else {
            return &self.image[(dpos - self.dpos_offset) as usize];
        }
    }

    pub fn get_block_mut(&mut self, dpos: i32) -> &mut AlignedBlock {
        self.fill_up_to_dpos(dpos);
        return &mut self.image[(dpos - self.dpos_offset) as usize];
    }

    #[inline(always)]
    pub fn get_blocks_mut(
        &mut self,
        above: i32,
        left: i32,
        above_left: i32,
        here: i32,
    ) -> (&[i16; 64], &[i16; 64], &[i16; 64], &mut AlignedBlock) {
        self.fill_up_to_dpos(here);

        let (first, rest) = self.image.split_at_mut((here - self.dpos_offset) as usize);

        return (
            if above == -1 {
                &EMPTY.get_block()
            } else {
                &first[(above - self.dpos_offset) as usize].get_block()
            },
            if left == -1 {
                &EMPTY.get_block()
            } else {
                &first[(left - self.dpos_offset) as usize].get_block()
            },
            if above_left == -1 {
                &EMPTY.get_block()
            } else {
                &first[(above_left - self.dpos_offset) as usize].get_block()
            },
            &mut rest[0],
        );
    }

    #[inline(always)]
    pub fn get_blocks(
        &self,
        above: i32,
        left: i32,
        above_left: i32,
        here: i32,
    ) -> (&[i16; 64], &[i16; 64], &[i16; 64], &AlignedBlock) {
        return (
            if above == -1 {
                &EMPTY.get_block()
            } else {
                &self.image[(above - self.dpos_offset) as usize].get_block()
            },
            if left == -1 {
                &EMPTY.get_block()
            } else {
                &self.image[(left - self.dpos_offset) as usize].get_block()
            },
            if above_left == -1 {
                &EMPTY.get_block()
            } else {
                &self.image[(above_left - self.dpos_offset) as usize].get_block()
            },
            &self.image[(here - self.dpos_offset) as usize],
        );
    }
}

/// block of 64 coefficients in the aligned order, which is similar to zigzag except that the 7x7 lower right square comes first,
/// followed by the DC, followed by the edges
pub struct AlignedBlock {
    raw_data: [i16; 64],
}

impl AlignedBlock {
    pub fn get_dc(&self) -> i16 {
        return self.raw_data[ALIGNED_BLOCK_INDEX_DC_INDEX];
    }

    pub fn set_dc(&mut self, value: i16) {
        self.raw_data[ALIGNED_BLOCK_INDEX_DC_INDEX] = value
    }

    pub fn set_coefficient_zigzag_block(block_data: &mut [i16; 64], index: u8, value: i16) {
        block_data[usize::from(crate::consts::ZIGZAG_TO_ALIGNED[usize::from(index)])] = value;
    }

    pub fn get_block(&self) -> &[i16; 64] {
        return &self.raw_data;
    }

    pub fn get_block_mut(&mut self) -> &mut [i16; 64] {
        return &mut self.raw_data;
    }

    // used for debugging
    #[allow(dead_code)]
    pub fn get_hash(&self) -> i32 {
        let mut sum = 0;
        for i in 0..64 {
            sum += self.raw_data[i] as i32
        }
        return sum;
    }

    pub fn get_count_of_non_zeros_7x7(&self) -> u8 {
        // with aligned (zigzag) arrangement, the 7x7 data is located in offsets 0..48
        let mut num_non_zeros7x7: u8 = 0;
        for index in 0..49 {
            if self.raw_data[index] != 0 {
                num_non_zeros7x7 += 1;
            }
        }

        return num_non_zeros7x7;
    }

    pub fn get_coefficient(&self, index: usize) -> i16 {
        return self.raw_data[index];
    }

    pub fn set_coefficient(&mut self, index: usize, v: i16) {
        self.raw_data[index] = v;
    }

    pub fn set_coefficient_zigzag(&mut self, index: usize, v: i16) {
        self.raw_data[usize::from(ZIGZAG_TO_ALIGNED[index])] = v;
    }

    pub fn get_coefficient_raster(&self, index: usize) -> i16 {
        return self.raw_data[usize::from(RASTER_TO_ALIGNED[index])];
    }

    pub fn get_coefficient_zigzag(&self, index: usize) -> i16 {
        return self.raw_data[usize::from(ZIGZAG_TO_ALIGNED[index])];
    }
}
