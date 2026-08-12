// 数字滚动动画：value 变化时从旧值经 spring 平滑滚动到新值
import React, { useEffect, useRef } from "react";
import { motion, useMotionValue, useSpring, useTransform } from "framer-motion";

export const AnimatedNumber: React.FC<{ value: number; className?: string }> = ({
  value,
  className,
}) => {
  // 首帧直接显示当前值（不做 0 → N 的初始滚动），后续变化才滚动
  const initialRef = useRef(value);
  const motionValue = useMotionValue(initialRef.current);
  const spring = useSpring(motionValue, { stiffness: 180, damping: 22 });
  const text = useTransform(spring, (v) => Math.round(v).toString());

  useEffect(() => {
    motionValue.set(value);
  }, [value, motionValue]);

  return <motion.span className={className}>{text}</motion.span>;
};
