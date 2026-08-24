import { Bar } from 'react-chartjs-2'
import {
  Chart as ChartJS,
  CategoryScale,
  LinearScale,
  BarElement,
  Tooltip,
  type ChartData,
  type ChartOptions,
} from 'chart.js'

ChartJS.register(CategoryScale, LinearScale, BarElement, Tooltip)

export type PulseChartProps = {
  data: ChartData<'bar'>
  options: ChartOptions<'bar'>
}

export default function PulseChart({ data, options }: PulseChartProps) {
  return <Bar data={data} options={options} />
}
