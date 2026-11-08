import type { Time } from "./types";
const gcd = (a: bigint, b: bigint): bigint => {
  a = a < 0n ? -a : a;
  while (b) {
    const r = a % b;
    a = b;
    b = r;
  }
  return a;
};
const exact = (ticks: bigint, timebase: bigint): Time => {
  const d = gcd(ticks, timebase),
    n = ticks / d,
    b = timebase / d;
  if (b < 1n || b > 4294967295n || n < -(2n ** 63n) || n > 2n ** 63n - 1n)
    throw new Error("Exact time exceeds public representation");
  return { ticks: n.toString(), timebase: Number(b) };
};
export const secondsForDisplay = (value: Time) =>
  Number.parseFloat(value.ticks) / value.timebase;
export const addTime = (a: Time, b: Time) =>
  exact(
    BigInt(a.ticks) * BigInt(b.timebase) + BigInt(b.ticks) * BigInt(a.timebase),
    BigInt(a.timebase) * BigInt(b.timebase),
  );
export const scaleTime = (value: Time, numerator: Time, denominator: Time) =>
  exact(
    BigInt(value.ticks) *
      BigInt(numerator.ticks) *
      BigInt(denominator.timebase),
    BigInt(value.timebase) *
      BigInt(numerator.timebase) *
      BigInt(denominator.ticks),
  );
export const subtractWholeSeconds = (value: Time, seconds: bigint) =>
  exact(
    BigInt(value.ticks) - seconds * BigInt(value.timebase),
    BigInt(value.timebase),
  );
export const halfTime = (value: Time) =>
  exact(BigInt(value.ticks), BigInt(value.timebase) * 2n);
export const compareTime = (a: Time, b: Time) => {
  const left = BigInt(a.ticks) * BigInt(b.timebase);
  const right = BigInt(b.ticks) * BigInt(a.timebase);
  return left < right ? -1 : left > right ? 1 : 0;
};
export const decimalSecondsHelp = "Enter seconds from 0 with up to 6 decimal places, for example 1.25.";
export const timeFromDecimalSeconds = (value: string): Time => {
  const match = /^(\d+)(?:\.(\d{0,6}))?$/.exec(value.trim());
  if (!match) throw new Error(decimalSecondsHelp);
  const fraction = match[2] ?? "",
    timebase = 10n ** BigInt(fraction.length),
    ticks = BigInt(match[1]) * timebase + BigInt(fraction || "0");
  return exact(ticks, timebase);
};

export const zero: Time = { ticks: "0", timebase: 1 };
export const fromSeconds = (seconds: number): Time => {
  if (!Number.isFinite(seconds)) throw new Error("Time must be finite");
  return exact(BigInt(Math.round(seconds * 120000)), 120000n);
};
export const atFrame = (
  seconds: number,
  fps: { numerator: number; denominator: number },
): Time =>
  exact(
    BigInt(
      Math.max(0, Math.round((seconds * fps.numerator) / fps.denominator)),
    ) * BigInt(fps.denominator),
    BigInt(fps.numerator),
  );
export const subtractTime = (a: Time, b: Time): Time =>
  addTime(a, { ticks: String(-BigInt(b.ticks)), timebase: b.timebase });
export const formatTime = (time: Time): string => {
  const seconds = Math.max(0, secondsForDisplay(time));
  return `${String(Math.floor(seconds / 60)).padStart(2, "0")}:${(seconds % 60).toFixed(2).padStart(5, "0")}`;
};
