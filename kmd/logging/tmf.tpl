`FORALL Msg IN Messages`
`Msg.Guid` Venera // SRC=`SourceFile.Name`
#typev `Msg.Name` `Msg.MsgNo` "`Msg.Text`" // `Msg.GooPairs`
{
`FORALL Arg IN Msg.Arguments`
`Arg.Name`, `Arg.MofType` -- `Arg.No`
`ENDFOR`
}
`ENDFOR`
