//! nnnoiseless DNN 降噪包装
//! 工作在 48kHz，每次处理 480 samples（10ms）

use nnnoiseless::DenoiseState;

const FRAME_SIZE: usize = 480;

pub struct Denoiser {
    state: Box<DenoiseState<'static>>,
    input_buffer: Vec<f32>,
}

impl Denoiser {
    pub fn new() -> Self {
        Self {
            state: DenoiseState::new(),
            input_buffer: Vec::with_capacity(FRAME_SIZE * 2),
        }
    }

    /// 输入 48kHz f32 samples（任意长度），返回等长降噪后的 samples。
    /// 不足一帧的尾部样本原样直通。
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        self.input_buffer.extend_from_slice(input);
        let mut output = Vec::with_capacity(input.len());

        while self.input_buffer.len() >= FRAME_SIZE {
            let frame: Vec<f32> = self.input_buffer.drain(..FRAME_SIZE).collect();
            let mut out = vec![0.0f32; FRAME_SIZE];
            self.state.process_frame(&mut out, &frame);
            output.extend_from_slice(&out);
        }

        // 不足一帧的尾部原样直通（保证输出长度 == 输入长度）
        output.extend_from_slice(&self.input_buffer);
        self.input_buffer.clear();

        output
    }
}

impl Default for Denoiser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_output_length_equals_input_full_frame() {
        let mut denoiser = Denoiser::new();
        let input = vec![0.1f32; 480];
        let output = denoiser.process(&input);
        assert_eq!(output.len(), input.len());
    }

    #[test]
    fn test_partial_frame_passthrough() {
        let mut denoiser = Denoiser::new();
        let input = vec![0.1f32; 200];
        let output = denoiser.process(&input);
        assert_eq!(output.len(), 200);
    }

    #[test]
    fn test_cross_frame_boundary() {
        let mut denoiser = Denoiser::new();
        let input = vec![0.05f32; 720];
        let output = denoiser.process(&input);
        assert_eq!(output.len(), 720);
    }

    #[test]
    fn test_silence_stays_near_zero() {
        let mut denoiser = Denoiser::new();
        let silence = vec![0.0f32; 4800];
        let output = denoiser.process(&silence);
        let max_abs = output.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
        assert!(max_abs < 0.01, "静音降噪后超出阈值: {}", max_abs);
    }
}
