import { useEffect } from 'react';
import type { ReactNode } from 'react';
import { useAuthStore } from '../../stores/authStore';
import { LoginForm } from './LoginForm';

interface RequireAuthProps {
  children: ReactNode;
}

/**
 * 认证包装组件
 *
 * 认证默认禁用 —— 用户直接进入应用。
 * 只有当检测到后端明确要求认证（API 返回 401）时，
 * 才显示登录界面。
 */
export function RequireAuth({ children }: RequireAuthProps) {
  const { isAuthenticated, checkAuth, error } = useAuthStore();

  // 定期检查认证状态（已登录用户）
  useEffect(() => {
    const interval = setInterval(() => {
      checkAuth();
    }, 60000);
    return () => clearInterval(interval);
  }, [checkAuth]);

  // 用户已认证 → 直接渲染应用
  if (isAuthenticated) {
    return <>{children}</>;
  }

  // 有认证错误（后端返回401） → 显示登录界面
  // 否则认证默认禁用，直接渲染应用
  if (error) {
    return (
      <LoginForm />
    );
  }

  return <>{children}</>;
}