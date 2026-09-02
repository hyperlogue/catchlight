/**
 * Every param the model carries, one row each.
 *
 * The list is a replica read, so it moves when the document does — a param
 * added by an agent on the socket appears here without this component knowing
 * anything happened. The render prop is the composition seam: a host that
 * wants a name, a value readout and a binding count writes the row, and still
 * gets the list.
 */

import type { ParamInfo, Session } from "@catchlight/core";
import type { ComponentProps, ReactNode } from "react";

import { ParamSliderRoot } from "./param-slider.js";
import { useParams } from "./replica.js";

export interface ParamListRootProps extends Omit<ComponentProps<"div">, "children"> {
  session: Session;
  children?: (param: ParamInfo) => ReactNode;
}

export function ParamListRoot({ session, children, ...rest }: ParamListRootProps) {
  const params = useParams(session);
  return (
    <div role="list" data-catchlight-param-list="" {...rest}>
      {params.map((param) => (
        <div role="listitem" data-catchlight-param-item="" data-param={param.id} key={param.id}>
          {children ? children(param) : <ParamSliderRoot session={session} param={param} />}
        </div>
      ))}
    </div>
  );
}

export const ParamList = { Root: ParamListRoot };
