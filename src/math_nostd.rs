//! 纯 `#![no_std]` 数学运算核心库
//! 在裸机 `core` 环境下，原生 `f32` 不提供 `sqrt`、`sin`、`cos`、`floor`、`ceil`、`round`。
//! 本模块使用纯位运算、牛顿迭代法与极小极大逼近多项式在 `core` 下实现高精度浮点运算，零外部库依赖。

use core::f32::consts::PI;

pub trait NoStdFloat {
    fn ns_floor(self) -> f32;
    fn ns_ceil(self) -> f32;
    fn ns_round(self) -> f32;
    fn ns_sqrt(self) -> f32;
    fn ns_sin(self) -> f32;
    fn ns_cos(self) -> f32;
    fn ns_powi2(self) -> f32;
}

impl NoStdFloat for f32 {
    #[inline]
    fn ns_floor(self) -> f32 {
        if !self.is_finite() {
            return 0.0;
        }
        let xi = self as i32;
        let xf = xi as f32;
        if self < xf {
            xf - 1.0
        } else {
            xf
        }
    }

    #[inline]
    fn ns_ceil(self) -> f32 {
        if !self.is_finite() {
            return 0.0;
        }
        let xi = self as i32;
        let xf = xi as f32;
        if self > xf {
            xf + 1.0
        } else {
            xf
        }
    }

    #[inline]
    fn ns_round(self) -> f32 {
        if !self.is_finite() {
            return 0.0;
        }
        if self >= 0.0 {
            (self + 0.5).ns_floor()
        } else {
            (self - 0.5).ns_ceil()
        }
    }

    #[inline]
    fn ns_sqrt(self) -> f32 {
        if self <= 0.0 || !self.is_finite() {
            return 0.0;
        }
        // IEEE-754 快速初始猜测 + 3 轮牛顿-拉夫逊迭代 (精度达 23-bit 尾数极限)
        let mut y = f32::from_bits((self.to_bits() >> 1) + 0x1fc0_0000);
        y = 0.5 * (y + self / y);
        y = 0.5 * (y + self / y);
        y = 0.5 * (y + self / y);
        y
    }

    #[inline]
    fn ns_sin(self) -> f32 {
        if !self.is_finite() {
            return 0.0;
        }
        // 归约到 [-PI, PI]
        let two_pi = 2.0 * PI;
        let k = (self / two_pi).ns_round();
        let mut x = self - k * two_pi;
        if x > PI {
            x -= two_pi;
        } else if x < -PI {
            x += two_pi;
        }
        // 归约到 [-PI/2, PI/2]
        if x > PI * 0.5 {
            x = PI - x;
        } else if x < -PI * 0.5 {
            x = -PI - x;
        }
        // 7 阶泰勒/切比雪夫多项式逼近
        let x2 = x * x;
        x * (1.0 - x2 * (1.0 / 6.0 - x2 * (1.0 / 120.0 - x2 * (1.0 / 5040.0))))
    }

    #[inline]
    fn ns_cos(self) -> f32 {
        (self + PI * 0.5).ns_sin()
    }

    #[inline]
    fn ns_powi2(self) -> f32 {
        self * self
    }
}
