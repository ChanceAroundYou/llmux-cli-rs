import React, { useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { useAuthStore } from '@/stores/auth'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Card, CardContent, CardHeader, CardTitle, CardDescription } from '@/components/ui/card'
import { Zap, Loader2 } from 'lucide-react'

export default function Login() {
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [error, setError] = useState('')
  const [loading, setLoading] = useState(false)
  const login = useAuthStore(s => s.login)
  const navigate = useNavigate()

  const onSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    setError('')
    if (!username.trim() || !password) {
      setError('请输入用户名和密码')
      return
    }
    setLoading(true)
    const ok = await login(username.trim(), password)
    setLoading(false)
    if (ok) {
      navigate('/', { replace: true })
    } else {
      setError('用户名或密码错误')
    }
  }

  return (
    <div className="min-h-[100dvh] flex items-center justify-center bg-background p-4 overflow-hidden">
      <Card className="w-full max-w-sm shadow-lg">
        <CardHeader className="space-y-2.5 text-center p-5 pb-3">
          <div className="mx-auto bg-primary text-primary-foreground p-2 rounded-lg w-fit">
            <Zap size={20} fill="currentColor" />
          </div>
          <CardTitle className="text-lg">登录 LLMux</CardTitle>
          <CardDescription>请输入管理员账号密码</CardDescription>
        </CardHeader>
        <CardContent className="p-5 pt-0">
          <form onSubmit={onSubmit} className="space-y-3">
            <div className="space-y-1.5">
              <label className="text-sm font-medium">用户名</label>
              <Input value={username} onChange={e => setUsername(e.target.value)} placeholder="请输入用户名" autoFocus />
            </div>
            <div className="space-y-1.5">
              <label className="text-sm font-medium">密码</label>
              <Input type="password" value={password} onChange={e => setPassword(e.target.value)} placeholder="请输入密码" />
            </div>
            {error && <div className="text-sm text-destructive bg-destructive/10 border border-destructive/20 rounded-md px-3 py-1.5">{error}</div>}
            <Button type="submit" className="w-full" disabled={loading}>
              {loading && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              登录
            </Button>
          </form>
        </CardContent>
      </Card>
    </div>
  )
}
