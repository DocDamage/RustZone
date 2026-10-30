#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FadeShape {
    Linear,
    Exponential,
    SCurve,
}

/// Top-and-Tail Trimming & Fade Parameters
#[derive(Clone, Copy, Debug)]
pub struct TrimFadeSettings {
    pub trim_start_fraction: f64, // 0.0 to 1.0
    pub trim_end_fraction: f64,   // 0.0 to 1.0
    pub fade_in_seconds: f64,     // Seconds of fade-in at start
    pub fade_out_seconds: f64,    // Seconds of fade-out at end
    pub fade_shape: FadeShape,
}

impl Default for TrimFadeSettings {
    fn default() -> Self {
        Self {
            trim_start_fraction: 0.0,
            trim_end_fraction: 1.0,
            fade_in_seconds: 0.0,
            fade_out_seconds: 0.0,
            fade_shape: FadeShape::SCurve,
        }
    }
}

impl TrimFadeSettings {
    /// Calculate exact integer frame boundaries for a given audio buffer length
    pub fn get_frame_boundaries(&self, total_frames: usize) -> (usize, usize) {
        if total_frames == 0 {
            return (0, 0);
        }
        let start = ((self.trim_start_fraction.clamp(0.0, 1.0)) * total_frames as f64) as usize;
        let end = ((self.trim_end_fraction.clamp(0.0, 1.0)) * total_frames as f64) as usize;
        let valid_start = start.min(total_frames);
        let valid_end = end.max(valid_start).min(total_frames);
        (valid_start, valid_end)
    }

    /// Calculate gain multiplier for the current playback/render frame taking trim and fades into account
    #[inline]
    pub fn calculate_gain(
        &self,
        current_frame: usize,
        start_frame: usize,
        end_frame: usize,
        sample_rate: u32,
    ) -> f32 {
        if current_frame < start_frame || current_frame >= end_frame {
            return 0.0;
        }

        let fade_in_frames = (self.fade_in_seconds * sample_rate as f64) as usize;
        let fade_out_frames = (self.fade_out_seconds * sample_rate as f64) as usize;

        // 1. Fade-In zone
        if fade_in_frames > 0 && current_frame < start_frame + fade_in_frames {
            let t = ((current_frame - start_frame) as f32 / fade_in_frames as f32).clamp(0.0, 1.0);
            return self.apply_shape(t);
        }

        // 2. Fade-Out zone
        if fade_out_frames > 0 && current_frame >= end_frame.saturating_sub(fade_out_frames) {
            let frames_left = end_frame.saturating_sub(current_frame);
            let t = (frames_left as f32 / fade_out_frames as f32).clamp(0.0, 1.0);
            return self.apply_shape(t);
        }

        1.0
    }

    #[inline]
    fn apply_shape(&self, t: f32) -> f32 {
        match self.fade_shape {
            FadeShape::Linear => t,
            FadeShape::Exponential => t * t,
            FadeShape::SCurve => 0.5 * (1.0 - (std::f32::consts::PI * t).cos()),
        }
    }
}

