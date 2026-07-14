//! cpal 输出流。
//!
//! 音频回调运行在实时线程上，里面不能加锁、不能分配、不能做 I/O，所以样本通过
//! 无锁环形缓冲递进去，欠载只累加一个原子计数器，由外面定期打日志。

use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// 环形缓冲深度，按设备采样率的 0.5 秒算
const BUFFER_SECONDS: f32 = 0.5;

pub struct Player {
    /// 持有即播放，drop 即停。不能省。
    _stream: cpal::Stream,
    prod: HeapProd<f32>,
    underruns: Arc<AtomicU64>,
    sample_rate: u32,
}

impl Player {
    pub fn new() -> Result<Self> {
        let device = cpal::default_host().default_output_device().context("找不到默认输出设备")?;
        let supported = device.default_output_config().context("读取默认输出配置失败")?;

        let sample_format = supported.sample_format();
        let channels = supported.channels() as usize;
        let sample_rate = supported.sample_rate();
        let config: cpal::StreamConfig = supported.into();

        tracing::info!("音频输出: {sample_rate}Hz {channels}ch {sample_format}");

        let capacity = (sample_rate as f32 * BUFFER_SECONDS) as usize;
        let (prod, cons) = HeapRb::<f32>::new(capacity).split();
        let underruns = Arc::new(AtomicU64::new(0));

        let stream = match sample_format {
            SampleFormat::F32 => build::<f32>(&device, config, cons, channels, underruns.clone()),
            SampleFormat::I16 => build::<i16>(&device, config, cons, channels, underruns.clone()),
            other => bail!("不支持的采样格式: {other}"),
        }?;
        stream.play().context("启动输出流失败")?;

        Ok(Self { _stream: stream, prod, underruns, sample_rate })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// 送入单声道样本。缓冲满时多余的会被丢掉，返回实际写入数。
    pub fn push(&mut self, samples: &[f32]) -> usize {
        self.prod.push_slice(samples)
    }

    /// 环形缓冲里还有多少样本没被播出去。
    ///
    /// 这是喂数据的唯一依据：音频设备按自己的晶振精确消费，和 tokio 的定时器是
    /// 两个独立时钟，永远对不齐。按时钟喂必然持续欠载或溢出，按水位喂则是设备
    /// 消费多少我们补多少，自动跟上它的节奏。
    pub fn buffered(&self) -> usize {
        self.prod.occupied_len()
    }

    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut cons: HeapCons<f32>,
    channels: usize,
    underruns: Arc<AtomicU64>,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    device
        .build_output_stream::<T, _, _>(
            config,
            move |data: &mut [T], _| {
                let mut missing = 0u64;
                // 单声道展开到设备的所有声道
                for frame in data.chunks_mut(channels) {
                    let s = cons.try_pop().unwrap_or_else(|| {
                        missing += 1;
                        0.0
                    });
                    frame.fill(T::from_sample(s));
                }
                if missing > 0 {
                    underruns.fetch_add(missing, Ordering::Relaxed);
                }
            },
            |err| tracing::error!("音频输出流错误: {err}"),
            None,
        )
        .context("创建输出流失败")
}
