using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class DesktopQA {
 delegate bool Callback(IntPtr h,IntPtr p);
 [DllImport("user32.dll")] static extern bool EnumWindows(Callback f,IntPtr p);
 [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr root,Callback f,IntPtr p);
 public class FieldSample {public long hwnd,root;public int pid,id;public string Class;public bool visible,enabled;}
 public static FieldSample[] FilenameFields(IntPtr dialog,int pid){
  if(dialog==IntPtr.Zero||Class(dialog)!="#32770"||Owner(dialog)!=pid)throw new InvalidOperationException("Open dialog identity differs");
  var matches=new List<FieldSample>();EnumChildWindows(dialog,(h,p)=>{int id=GetDlgCtrlID(h);if(id==1148)matches.Add(new FieldSample{hwnd=h.ToInt64(),root=GetAncestor(h,2).ToInt64(),pid=Owner(h),id=id,Class=Class(h),visible=IsWindowVisible(h),enabled=IsWindowEnabled(h)});return true;},IntPtr.Zero);
  return matches.ToArray();
 }
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
 [DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr h,uint cmd);
 [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr h,uint flags);
 [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h,out uint pid);
 [DllImport("user32.dll",CharSet=CharSet.Unicode)] static extern int GetClassName(IntPtr h,StringBuilder s,int n);
 [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
 [StructLayout(LayoutKind.Sequential)] struct Gui {public int size,flags; public IntPtr active,focus,capture,menu,move,caret; public int l,t,r,b;}
 [DllImport("user32.dll")] static extern bool GetGUIThreadInfo(uint tid,ref Gui info);
 [DllImport("user32.dll",CharSet=CharSet.Unicode)] static extern IntPtr SendMessageTimeout(IntPtr h,uint msg,IntPtr w,IntPtr l,uint flags,uint timeout,out IntPtr result);
 public static IntPtr[] Windows(){var a=new List<IntPtr>();if(!EnumWindows((h,p)=>{a.Add(h);return true;},IntPtr.Zero))throw new InvalidOperationException("Window enumeration failed");return a.ToArray();}
 [DllImport("user32.dll")] public static extern IntPtr GetDlgItem(IntPtr h,int id);
 [DllImport("user32.dll")] public static extern bool IsWindowEnabled(IntPtr h);
 public static string ReadFilename(IntPtr field,int expectedLength){
  int capacity=expectedLength+2;IntPtr b=Marshal.AllocHGlobal(capacity*2);
  try{IntPtr n;if(SendMessageTimeout(field,0xd,(IntPtr)capacity,b,2,2000,out n)==IntPtr.Zero)throw new TimeoutException("Filename read timed out");return Marshal.PtrToStringUni(b,n.ToInt32());}finally{Marshal.FreeHGlobal(b);}
 }
 public static bool NeedsRefill(string actual,string expected,bool used){
  if(actual==expected)return false;
  if(actual==String.Empty&&!used)return true;
  throw new InvalidOperationException("Filename differs at click boundary");
 }
 public static void SetTextVerified(IntPtr h,string value){IntPtr data=Marshal.StringToHGlobalUni(value);IntPtr b=Marshal.AllocHGlobal((value.Length+2)*2);try{IntPtr r;if(SendMessageTimeout(h,0xc,IntPtr.Zero,data,2,2000,out r)==IntPtr.Zero||r==IntPtr.Zero)throw new InvalidOperationException("Filename set failed");if(SendMessageTimeout(h,0xd,(IntPtr)(value.Length+2),b,2,2000,out r)==IntPtr.Zero||Marshal.PtrToStringUni(b)!=value)throw new InvalidOperationException("Filename readback differs");}finally{Marshal.FreeHGlobal(data);Marshal.FreeHGlobal(b);}}
 public static void Click(IntPtr h){IntPtr r;if(SendMessageTimeout(h,0xf5,IntPtr.Zero,IntPtr.Zero,2,3000,out r)==IntPtr.Zero)throw new TimeoutException("Open button response timed out");}
 public static int Owner(IntPtr h){uint p;GetWindowThreadProcessId(h,out p);return (int)p;}
 public static string Class(IntPtr h){var s=new StringBuilder(256);GetClassName(h,s,256);return s.ToString();}
 [DllImport("user32.dll")] public static extern int GetDlgCtrlID(IntPtr h);
 public static IntPtr Focused(IntPtr win){uint pid;uint tid=GetWindowThreadProcessId(win,out pid);var g=new Gui();g.size=Marshal.SizeOf(g);return tid!=0&&GetGUIThreadInfo(tid,ref g)?g.focus:IntPtr.Zero;}
 [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
 public static bool InWindow(IntPtr fg,IntPtr root,int pid){return fg!=IntPtr.Zero&&Owner(fg)==pid&&GetAncestor(fg,2)==root;}
 public static void Edit(int pid,IntPtr root,IntPtr edit,string text){
  for(int i=-1;i<text.Length;i++){
   uint p;uint tid=GetWindowThreadProcessId(edit,out p);var g=new Gui();g.size=Marshal.SizeOf(g);
   if(tid==0||p!=pid||Owner(root)!=pid||GetAncestor(edit,2)!=root||Class(edit)!="RichEditD2DPT"||!IsWindowVisible(edit)||!InWindow(GetForegroundWindow(),root,pid)||!GetGUIThreadInfo(tid,ref g)||g.focus!=edit)throw new InvalidOperationException("Editor focus changed");
   IntPtr result;if(SendMessageTimeout(edit,i<0?0xb1u:0x102u,i<0?IntPtr.Zero:(IntPtr)(int)text[i],(IntPtr)(i<0?-1:1),2,1500,out result)==IntPtr.Zero)throw new TimeoutException("Editor response timed out");
  }
 }
}
